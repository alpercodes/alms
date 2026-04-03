//! Command execution for the shell tool.
//!
//! Handles spawning processes, capturing output, enforcing timeouts,
//! and detecting the post-execution working directory via a `pwd` marker.

use super::output::truncate_output;
use super::security::{
    argv_references_denied_file, command_matches_denylist, command_references_denied_file,
    is_secret_env_var, platform_critical_env_vars,
};
use super::types::{ShellInput, ShellOutput, ShellState};
use crate::{SandboxError, error::SandboxResult};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, instrument, warn};

/// Unique marker used to delimit the `pwd` output appended after each command.
/// Must be unlikely to appear in normal command output.
const PWD_MARKER: &str = "__ALMS_PWD_MARKER__";

/// Execute a shell command synchronously (foreground).
///
/// This is the main execution path. It:
/// 1. Validates the command against security checks
/// 2. Builds the process command with cwd, env vars, and timeout
/// 3. Spawns the process and waits for completion
/// 4. Detects the new cwd from the `pwd` marker in output
/// 5. Truncates output if needed
/// 6. Updates the shell state's cwd
#[instrument(level = "debug", skip(state, sandbox_root, default_env), fields(timeout_ms = input.timeout_ms))]
pub(crate) async fn execute_command(
    input: &ShellInput,
    state: &ShellState,
    sandbox_root: Option<&Path>,
    unrestricted: bool,
    default_env: &HashMap<String, String>,
) -> SandboxResult<ShellOutput> {
    // Determine the effective command string and argv for security checks
    let (effective_command, is_command_mode) = resolve_command(input)?;

    // Security: check for denied files
    if is_command_mode {
        if let Some(denied) = command_references_denied_file(&effective_command) {
            return Err(SandboxError::SandboxViolation(format!(
                "Command references denied file '{denied}'"
            )));
        }
        if let Some(pattern) = command_matches_denylist(&effective_command) {
            return Err(SandboxError::SandboxViolation(format!(
                "Command matches denied pattern '{pattern}'"
            )));
        }
    } else {
        // argv mode: validate each argument
        let argv_strs: Vec<&str> = effective_command.split('\0').collect();
        if let Some(denied) = argv_references_denied_file(&argv_strs) {
            return Err(SandboxError::SandboxViolation(format!(
                "Command references denied file '{denied}'"
            )));
        }
    }

    let cwd = state.cwd.lock().await.clone();

    // Validate cwd against sandbox
    if !unrestricted && let Some(root) = sandbox_root {
        validate_cwd(&cwd, root)?;
    }

    // Build and spawn the command
    let mut cmd = build_command(&effective_command, is_command_mode, &cwd)?;

    // Configure environment
    configure_env(&mut cmd, default_env);

    // Set timeout
    let timeout = std::time::Duration::from_millis(input.timeout_ms);

    debug!(
        command = %effective_command,
        cwd = %cwd.display(),
        timeout_ms = input.timeout_ms,
        "Spawning shell command"
    );

    let child = cmd
        .spawn()
        .map_err(|e| SandboxError::Io(format!("Failed to spawn command: {e}")))?;

    let result = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            SandboxError::Io(format!(
                "Process timed out after {}s",
                input.timeout_ms / 1000
            ))
        })?
        .map_err(|e| SandboxError::Io(format!("Process error: {e}")))?;

    let exit_code = result.status.code().unwrap_or(-1);
    let raw_stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let raw_stderr = String::from_utf8_lossy(&result.stderr).to_string();

    // Extract cwd from pwd marker if we used command mode
    if is_command_mode && let Some(new_cwd) = extract_cwd_from_output(&raw_stdout) {
        let mut cwd_lock = state.cwd.lock().await;
        *cwd_lock = PathBuf::from(new_cwd);
    }

    // Strip the pwd marker from stdout before returning
    let clean_stdout = strip_pwd_marker(&raw_stdout);

    // Truncate output
    let stdout = truncate_output(&clean_stdout);
    let stderr = truncate_output(&raw_stderr);

    Ok(ShellOutput {
        exit_code,
        stdout,
        stderr,
    })
}

/// Resolve the effective command from the input.
///
/// Returns `(command_string, is_command_mode)`.
/// In command mode, the string is the shell command.
/// In argv mode, the string is null-byte-joined argv elements (for security checks).
fn resolve_command(input: &ShellInput) -> SandboxResult<(String, bool)> {
    if let Some(ref command) = input.command {
        if command.trim().is_empty() {
            return Err(SandboxError::InvalidParameters(
                "'command' must not be empty".to_string(),
            ));
        }
        Ok((command.clone(), true))
    } else if let Some(ref argv) = input.argv {
        if argv.is_empty() {
            return Err(SandboxError::InvalidParameters(
                "'argv' must not be empty".to_string(),
            ));
        }
        // Join with null bytes for security check, but we'll use the Vec directly for spawning
        Ok((argv.join("\0"), false))
    } else {
        Err(SandboxError::InvalidParameters(
            "Either 'command' or 'argv' is required".to_string(),
        ))
    }
}

/// Build the tokio Command for execution.
///
/// In command mode, wraps with `bash -c` (Unix) or `cmd /c` (Windows)
/// and appends a `pwd` marker to detect cwd changes.
/// In argv mode, spawns the program directly.
fn build_command(
    effective_command: &str,
    is_command_mode: bool,
    cwd: &Path,
) -> SandboxResult<tokio::process::Command> {
    let mut cmd;

    if is_command_mode {
        // Command string mode: wrap with shell
        let wrapped = wrap_command_with_pwd_marker(effective_command);

        #[cfg(unix)]
        {
            cmd = tokio::process::Command::new("bash");
            cmd.arg("-c");
            cmd.arg(&wrapped);
        }
        #[cfg(windows)]
        {
            cmd = tokio::process::Command::new("bash");
            cmd.arg("-c");
            cmd.arg(&wrapped);
        }
    } else {
        // Argv mode: spawn directly
        let argv: Vec<&str> = effective_command.split('\0').collect();
        cmd = tokio::process::Command::new(argv[0]);
        if argv.len() > 1 {
            cmd.args(&argv[1..]);
        }
    }

    cmd.current_dir(cwd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    Ok(cmd)
}

/// Wrap a command string with a pwd marker so we can detect cwd changes.
///
/// The marker is appended after the command via `; echo <MARKER>; pwd`.
/// This way, even if the command fails, we still get the working directory.
fn wrap_command_with_pwd_marker(command: &str) -> String {
    // Use a subshell to capture the command's output, then echo the marker
    // and the current working directory.
    format!("{command}; __alms_ec=$?; echo; echo '{PWD_MARKER}'; pwd; exit $__alms_ec")
}

/// Extract the cwd from command output by looking for the PWD marker.
///
/// Returns `Some(path_str)` if found, `None` if the marker is not present.
fn extract_cwd_from_output(stdout: &str) -> Option<&str> {
    // Find the marker line and take the next line as the cwd
    let mut found_marker = false;
    for line in stdout.lines() {
        if found_marker {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
        if line.trim() == PWD_MARKER {
            found_marker = true;
        }
    }
    None
}

/// Strip the pwd marker and the cwd line from stdout before returning to the caller.
fn strip_pwd_marker(stdout: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut skip_remaining = false;

    for line in stdout.lines() {
        if line.trim() == PWD_MARKER {
            // Remove the trailing empty line before the marker if present
            if let Some(last) = lines.last()
                && last.trim().is_empty()
            {
                lines.pop();
            }
            skip_remaining = true;
            continue;
        }
        if skip_remaining {
            // Skip the pwd output line after the marker
            skip_remaining = false;
            continue;
        }
        lines.push(line);
    }

    lines.join("\n")
}

/// Validate that a cwd is within the sandbox root.
fn validate_cwd(cwd: &Path, sandbox_root: &Path) -> SandboxResult<()> {
    // Canonicalize both for comparison
    let canonical_root =
        std::fs::canonicalize(sandbox_root).unwrap_or_else(|_| sandbox_root.to_path_buf());
    let canonical_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());

    if !canonical_cwd.starts_with(&canonical_root) {
        return Err(SandboxError::SandboxViolation(format!(
            "Working directory '{}' is outside sandbox root '{}'",
            cwd.display(),
            sandbox_root.display()
        )));
    }
    Ok(())
}

/// Configure the environment for a spawned process.
///
/// 1. Clears all inherited env vars (prevents secret leakage)
/// 2. Re-injects platform-critical vars (PATH, SystemRoot, etc.)
/// 3. Injects default_env vars (ALMS_DATA_DIR, etc.)
/// 4. Filters out secret env vars from all sources
fn configure_env(cmd: &mut tokio::process::Command, default_env: &HashMap<String, String>) {
    cmd.env_clear();

    // Re-inject platform-critical env vars
    for key in platform_critical_env_vars() {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }

    // Inject default env vars (filtering secrets)
    for (k, v) in default_env {
        if is_secret_env_var(k) {
            warn!(env_var = %k, "Blocked secret env var from default_env injection");
            continue;
        }
        cmd.env(k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_command_with_pwd_marker() {
        let wrapped = wrap_command_with_pwd_marker("ls -la");
        assert!(wrapped.contains("ls -la"));
        assert!(wrapped.contains(PWD_MARKER));
        assert!(wrapped.contains("pwd"));
    }

    #[test]
    fn test_extract_cwd_from_output() {
        let output = format!("file1.txt\nfile2.txt\n\n{PWD_MARKER}\n/home/user/project\n");
        assert_eq!(extract_cwd_from_output(&output), Some("/home/user/project"));
    }

    #[test]
    fn test_extract_cwd_no_marker() {
        let output = "file1.txt\nfile2.txt\n";
        assert_eq!(extract_cwd_from_output(output), None);
    }

    #[test]
    fn test_strip_pwd_marker() {
        let output = format!("file1.txt\nfile2.txt\n\n{PWD_MARKER}\n/home/user\n");
        let cleaned = strip_pwd_marker(&output);
        assert_eq!(cleaned, "file1.txt\nfile2.txt");
        assert!(!cleaned.contains(PWD_MARKER));
        assert!(!cleaned.contains("/home/user"));
    }

    #[test]
    fn test_strip_pwd_marker_no_marker() {
        let output = "line1\nline2\n";
        let cleaned = strip_pwd_marker(output);
        // lines() strips trailing newline, join re-joins without it
        assert_eq!(cleaned, "line1\nline2");
    }

    #[test]
    fn test_resolve_command_string() {
        let input = ShellInput {
            command: Some("ls -la".to_string()),
            argv: None,
            description: None,
            timeout_ms: 120_000,
            run_in_background: false,
        };
        let (cmd, is_cmd) = resolve_command(&input).unwrap();
        assert_eq!(cmd, "ls -la");
        assert!(is_cmd);
    }

    #[test]
    fn test_resolve_command_argv() {
        let input = ShellInput {
            command: None,
            argv: Some(vec!["ls".to_string(), "-la".to_string()]),
            description: None,
            timeout_ms: 120_000,
            run_in_background: false,
        };
        let (cmd, is_cmd) = resolve_command(&input).unwrap();
        assert_eq!(cmd, "ls\0-la");
        assert!(!is_cmd);
    }

    #[test]
    fn test_resolve_command_prefers_command_over_argv() {
        let input = ShellInput {
            command: Some("echo hello".to_string()),
            argv: Some(vec!["ls".to_string()]),
            description: None,
            timeout_ms: 120_000,
            run_in_background: false,
        };
        let (cmd, is_cmd) = resolve_command(&input).unwrap();
        assert_eq!(cmd, "echo hello");
        assert!(is_cmd);
    }

    #[test]
    fn test_resolve_command_empty_command() {
        let input = ShellInput {
            command: Some("   ".to_string()),
            argv: None,
            description: None,
            timeout_ms: 120_000,
            run_in_background: false,
        };
        assert!(resolve_command(&input).is_err());
    }

    #[test]
    fn test_resolve_command_empty_argv() {
        let input = ShellInput {
            command: None,
            argv: Some(vec![]),
            description: None,
            timeout_ms: 120_000,
            run_in_background: false,
        };
        assert!(resolve_command(&input).is_err());
    }

    #[test]
    fn test_resolve_command_neither() {
        let input = ShellInput {
            command: None,
            argv: None,
            description: None,
            timeout_ms: 120_000,
            run_in_background: false,
        };
        assert!(resolve_command(&input).is_err());
    }

    #[test]
    fn test_validate_cwd_inside_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let cwd = root.join("subdir");
        std::fs::create_dir_all(&cwd).unwrap();
        assert!(validate_cwd(&cwd, &root).is_ok());
    }

    #[test]
    fn test_validate_cwd_outside_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        #[cfg(unix)]
        {
            assert!(validate_cwd(Path::new("/etc"), &root).is_err());
        }
        #[cfg(windows)]
        {
            assert!(validate_cwd(Path::new("C:\\Windows"), &root).is_err());
        }
    }
}
