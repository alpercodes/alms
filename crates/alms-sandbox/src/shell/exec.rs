//! Command execution for the shell tool.
//!
//! Handles spawning processes, capturing output, enforcing timeouts,
//! and detecting the post-execution working directory via a `pwd` marker.

use super::output::truncate_output_bytes;
use super::security::{command_matches_denylist, is_secret_env_var, platform_critical_env_vars};
use super::spill::{ShellSpillPolicy, write_spill};
use super::types::{MAX_OUTPUT_BYTES, ShellInput, ShellOutput, ShellState};
use crate::{SandboxError, error::SandboxResult};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, error, instrument, warn};

/// First ~100 chars of a command, intended for structured log fields.
///
/// Kept short so operator-visible logs never accidentally leak the tail of a
/// long command (which may contain secrets, base64 blobs, etc.).
pub(crate) fn command_excerpt(command: &str) -> String {
    const MAX_LEN: usize = 100;
    if command.len() <= MAX_LEN {
        command.to_string()
    } else {
        // Truncate at a char boundary; appending an ellipsis makes it clear
        // the log is a summary, not the full command.
        let mut end = MAX_LEN;
        while !command.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &command[..end])
    }
}

/// Execute a shell command synchronously (foreground).
///
/// This is the main execution path. It:
/// 1. Validates the command against security checks
/// 2. Builds the process command with cwd, env vars, and timeout
/// 3. Spawns the process and waits for completion
/// 4. Detects the new cwd from the `pwd` marker in output
/// 5. Truncates output if needed
/// 6. Updates the shell state's cwd
///
/// When `spill_policy.is_active()` and the captured stdout or stderr exceeds
/// `MAX_OUTPUT_BYTES`, the full pre-truncation bytes are written to
/// `{run_dir}/shell_{tool_call_id}.log` (issue #756). The returned
/// [`ShellOutput::spill_path`] carries the absolute path so the caller can
/// surface it to the agent.
// The per-execution argument shape intentionally kept flat for readability:
// bundling these into a struct obscures the call path from `ShellTool::execute`
// and `background::submit_background_task` without removing any real
// complexity (every field has a distinct lifetime / ownership story).
#[allow(clippy::too_many_arguments)]
#[instrument(level = "debug", skip(state, sandbox_root, default_env, pwd_marker, spill_policy, tool_call_id), fields(timeout_ms = input.timeout_ms))]
pub(crate) async fn execute_command(
    input: &ShellInput,
    state: &ShellState,
    sandbox_root: Option<&Path>,
    unrestricted: bool,
    default_env: &HashMap<String, String>,
    pwd_marker: &str,
    spill_policy: &ShellSpillPolicy,
    tool_call_id: &str,
) -> SandboxResult<ShellOutput> {
    let command = &input.command;

    if command.trim().is_empty() {
        return Err(SandboxError::InvalidParameters(
            "'command' must not be empty".to_string(),
        ));
    }

    // Security: check for destructive command patterns
    if let Some(pattern) = command_matches_denylist(command) {
        error!(
            tool = "shell",
            reason = "denylist_match",
            pattern = %pattern,
            command_excerpt = %command_excerpt(command),
            "Shell command matched hardcoded denylist"
        );
        return Err(SandboxError::SandboxViolation(format!(
            "Command matches denied pattern '{pattern}'"
        )));
    }

    let cwd = state.cwd.lock().await.clone();

    // Validate cwd against sandbox
    if !unrestricted && let Some(root) = sandbox_root {
        validate_cwd(&cwd, root)?;
    }

    // Build and spawn the command
    let mut cmd = build_command(command, &cwd, sandbox_root, unrestricted, pwd_marker)?;

    // Configure environment
    configure_env(&mut cmd, default_env);

    // Set timeout
    let timeout = std::time::Duration::from_millis(input.timeout_ms);

    debug!(
        command = %command,
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

    // Keep stdout/stderr as bytes through cwd extraction and truncation.
    // Lossy UTF-8 decoding happens only at the boundary, so binary-ish output
    // and Windows `\r` are preserved through the pipeline rather than being
    // rewritten to U+FFFD replacement chars mid-flight.
    let raw_stdout = result.stdout;
    let raw_stderr = result.stderr;

    // Extract cwd from pwd marker. The marker and the pwd output are always
    // ASCII so decoding just the stdout bytes as UTF-8 (lossy) for the
    // marker search is safe — replacement chars in user output cannot match
    // the marker pattern.
    {
        let stdout_for_marker = String::from_utf8_lossy(&raw_stdout);
        if let Some(new_cwd) = extract_cwd_from_output(&stdout_for_marker, pwd_marker) {
            let new_path = PathBuf::from(new_cwd);
            // Validate the new cwd against sandbox root before storing.
            // If the command changed directory outside the sandbox (e.g. `cd /etc`),
            // keep the old cwd to prevent sandbox escape on subsequent calls.
            if !unrestricted && let Some(root) = sandbox_root {
                if validate_cwd(&new_path, root).is_ok() {
                    let mut cwd_lock = state.cwd.lock().await;
                    *cwd_lock = new_path;
                } else {
                    warn!(
                        new_cwd = %new_path.display(),
                        sandbox_root = %root.display(),
                        "Post-command cwd is outside sandbox root; keeping previous cwd"
                    );
                }
            } else {
                let mut cwd_lock = state.cwd.lock().await;
                *cwd_lock = new_path;
            }
        }
    }

    // Strip the pwd marker from stdout before truncation so the marker line
    // and cwd line don't count toward the byte budget. Operates on raw bytes.
    let clean_stdout_bytes = strip_pwd_marker_bytes(&raw_stdout, pwd_marker);

    // Spill decision (issue #756): made *before* truncation so we have the
    // full bytes the agent might want to recover. The pre-truncation sizes
    // are what truncate_output_bytes will react to, so checking `> MAX_OUTPUT_BYTES`
    // here matches truncation's own entry condition exactly.
    let stdout_needs_spill = clean_stdout_bytes.len() > MAX_OUTPUT_BYTES;
    let stderr_needs_spill = raw_stderr.len() > MAX_OUTPUT_BYTES;
    let spill_path = if spill_policy.is_active() && (stdout_needs_spill || stderr_needs_spill) {
        // Unwrap is safe: is_active() guarantees run_dir is Some.
        let run_dir = spill_policy
            .run_dir
            .as_deref()
            .expect("is_active() implies run_dir");
        match write_spill(run_dir, tool_call_id, &clean_stdout_bytes, &raw_stderr) {
            Ok(path) => Some(path),
            Err(e) => {
                // Spill failure is non-fatal — the agent still gets the
                // truncated output, just without the recovery file.
                warn!(
                    error = %e,
                    run_dir = %run_dir.display(),
                    tool_call_id = %tool_call_id,
                    "Failed to write shell output spill file; falling back to truncated output only"
                );
                None
            }
        }
    } else {
        None
    };

    // Truncate raw bytes, then decode at the boundary.
    let stdout_truncated = truncate_output_bytes(&clean_stdout_bytes);
    let stderr_truncated = truncate_output_bytes(&raw_stderr);

    // Final lossy decode at the boundary. This is the only place U+FFFD is
    // substituted for invalid UTF-8 — everything upstream stays as bytes.
    let stdout = String::from_utf8_lossy(&stdout_truncated).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_truncated).into_owned();

    Ok(ShellOutput {
        exit_code,
        stdout,
        stderr,
        spill_path,
    })
}

/// Build the tokio Command for execution.
///
/// Wraps the command with `bash -c` on all platforms. On Windows, this
/// requires Git Bash or WSL to be available on PATH. We use bash (not
/// `cmd /c`) because the pwd marker, exit code capture, and shell syntax
/// all assume POSIX semantics.
///
/// On Linux 5.13+ with a sandbox root configured, Landlock filesystem
/// restrictions are applied via `pre_exec` so the child process can only
/// access files within the sandbox root.
fn build_command(
    command: &str,
    cwd: &Path,
    sandbox_root: Option<&Path>,
    unrestricted: bool,
    pwd_marker: &str,
) -> SandboxResult<tokio::process::Command> {
    let wrapped = wrap_command_with_pwd_marker(command, pwd_marker);

    #[cfg(windows)]
    let bash_bin = resolve_bash_path().clone();
    #[cfg(not(windows))]
    let bash_bin = PathBuf::from("bash");

    let mut cmd = tokio::process::Command::new(bash_bin);
    cmd.arg("-c");
    cmd.arg(&wrapped);

    cmd.current_dir(cwd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    // Apply Landlock filesystem sandboxing on Linux when a sandbox root is configured
    #[cfg(target_os = "linux")]
    if !unrestricted && let Some(root) = sandbox_root {
        apply_landlock_sandbox(&mut cmd, root);
    }

    // Suppress unused variable warnings on non-Linux platforms
    #[cfg(not(target_os = "linux"))]
    {
        let _ = sandbox_root;
        let _ = unrestricted;
    }

    Ok(cmd)
}

/// Resolve the Git-Bash executable once per process and cache the result.
///
/// `discover_bash_path` performs up to ~10 `Path::exists()` syscalls walking
/// the well-known Git-for-Windows install locations. Since the answer is stable
/// for the lifetime of the process, the resolution is memoized in a `LazyLock`
/// so `build_command` pays the stat cost only on the first invocation.
#[cfg(windows)]
fn resolve_bash_path() -> &'static PathBuf {
    static BASH_PATH: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(discover_bash_path);
    &BASH_PATH
}

#[cfg(windows)]
fn discover_bash_path() -> PathBuf {
    let paths = [
        "C:\\Program Files\\Git\\bin\\bash.exe",
        "C:\\Program Files\\Git\\usr\\bin\\bash.exe",
        "C:\\Program Files (x86)\\Git\\bin\\bash.exe",
        "C:\\Program Files (x86)\\Git\\usr\\bin\\bash.exe",
    ];
    for p in &paths {
        let path = Path::new(p);
        if path.exists() {
            return path.to_path_buf();
        }
    }

    if let Ok(pf) = std::env::var("ProgramFiles") {
        let path = Path::new(&pf).join("Git").join("bin").join("bash.exe");
        if path.exists() {
            return path;
        }
        let path = Path::new(&pf)
            .join("Git")
            .join("usr")
            .join("bin")
            .join("bash.exe");
        if path.exists() {
            return path;
        }
    }

    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        let path = Path::new(&pf86).join("Git").join("bin").join("bash.exe");
        if path.exists() {
            return path;
        }
        let path = Path::new(&pf86)
            .join("Git")
            .join("usr")
            .join("bin")
            .join("bash.exe");
        if path.exists() {
            return path;
        }
    }

    if let Ok(local) = std::env::var("LocalAppData") {
        let path = Path::new(&local)
            .join("Programs")
            .join("Git")
            .join("bin")
            .join("bash.exe");
        if path.exists() {
            return path;
        }
    }

    if let Ok(up) = std::env::var("USERPROFILE") {
        let path = Path::new(&up)
            .join("AppData")
            .join("Local")
            .join("Programs")
            .join("Git")
            .join("bin")
            .join("bash.exe");
        if path.exists() {
            return path;
        }
    }

    PathBuf::from("bash")
}

/// Apply Landlock filesystem restrictions to a command via `pre_exec`.
///
/// This restricts the child process (and all its descendants) to only
/// access files under `sandbox_root`. The restriction is applied using
/// Linux's Landlock LSM (available since Linux 5.13).
///
/// If Landlock is not supported by the running kernel, a warning is logged
/// and execution continues without filesystem restrictions.
#[cfg(target_os = "linux")]
fn apply_landlock_sandbox(cmd: &mut tokio::process::Command, sandbox_root: &Path) {
    // Canonicalize the sandbox root so the Landlock rules match real paths.
    // If canonicalization fails (dir doesn't exist yet), fall back to the
    // provided path — Landlock will simply deny all access, which is safer
    // than allowing everything.
    let canonical_root =
        std::fs::canonicalize(sandbox_root).unwrap_or_else(|_| sandbox_root.to_path_buf());

    // We also need to allow access to standard system paths so bash and
    // basic utilities can execute. These are read-only.
    //
    // NOTE: /etc is narrowed to specific entries that bash/coreutils need.
    // /tmp is excluded — commands that need temp space should use the sandbox root.
    //
    // /etc/passwd is intentionally NOT granted (issue #743 / #734 item 2):
    // exposing it enables user enumeration on shared hosts. The trade-off is
    // that `~user` tilde expansion to *other* users' home directories no
    // longer works, and `ls -l`/`whoami` may print numeric UIDs instead of
    // names. `~/path` (current user) still works because bash uses `$HOME`
    // for that, which doesn't read /etc/passwd. See `docs/security-model.md`
    // for the full rationale.
    let system_read_paths: Vec<PathBuf> = vec![
        PathBuf::from("/usr"),
        PathBuf::from("/bin"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        // Specific /etc entries needed by the dynamic linker and coreutils:
        PathBuf::from("/etc/ld.so.cache"),
        PathBuf::from("/etc/ld.so.conf"),
        PathBuf::from("/etc/ld.so.conf.d"),
        PathBuf::from("/etc/alternatives"),
        PathBuf::from("/etc/nsswitch.conf"),
        PathBuf::from("/etc/resolv.conf"),
        PathBuf::from("/etc/localtime"),
        PathBuf::from("/dev/null"),
        PathBuf::from("/dev/urandom"),
        PathBuf::from("/dev/zero"),
        PathBuf::from("/proc/self"),
    ];

    // Clone what we need into the closure (pre_exec runs after fork, before exec)
    let root_for_closure = canonical_root.clone();
    let sys_paths = system_read_paths;

    // SAFETY: pre_exec runs in the child process after fork() but before
    // exec(). The Landlock syscalls themselves are async-signal-safe, but
    // we use eprintln! for diagnostics on error paths, which is technically
    // not async-signal-safe (it may allocate). In practice this is reliable
    // after fork() on Linux and only executes on error paths. The trade-off
    // is accepted for debuggability.
    unsafe {
        cmd.pre_exec(move || {
            use landlock::{
                ABI, Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr,
                RulesetCreatedAttr, RulesetStatus,
            };

            // Request V5 features; the landlock crate will degrade to the
            // highest ABI the running kernel supports (best-effort).
            let abi = ABI::V5;

            let read_access = AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute;

            // NOTE: AccessFs::Refer is intentionally not granted for any path.
            // This means rename()/link() across differently-ruled directories
            // will be denied. rename() within the sandbox root works because
            // source and target share the same ruleset entry. This prevents
            // using rename to move files from system paths into the sandbox.
            let write_access = read_access
                | AccessFs::WriteFile
                | AccessFs::MakeReg
                | AccessFs::MakeDir
                | AccessFs::MakeSym
                | AccessFs::RemoveFile
                | AccessFs::RemoveDir
                | AccessFs::Truncate;

            let ruleset = match Ruleset::default().handle_access(AccessFs::from_all(abi)) {
                Ok(r) => r,
                Err(e) => {
                    // handle_access() failure means Landlock is not supported
                    // by this kernel — gracefully degrade to unsandboxed execution.
                    eprintln!("[alms] Landlock not supported by kernel, running unsandboxed: {e}");
                    return Ok(());
                }
            };

            // From this point on, the kernel supports Landlock. Any failure
            // to create or enforce the ruleset is a hard error — we must NOT
            // run the command unsandboxed when sandboxing was requested and
            // the kernel confirmed it can apply rules.

            let ruleset = match ruleset.create() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[alms] Landlock ruleset create failed: {e}");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("Landlock ruleset creation failed: {e}"),
                    ));
                }
            };

            // Allow full read+write access to the sandbox root
            let fd = match PathFd::new(&root_for_closure) {
                Ok(fd) => fd,
                Err(e) => {
                    eprintln!(
                        "[alms] Landlock: cannot open sandbox root '{}': {e}",
                        root_for_closure.display()
                    );
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "Landlock: cannot open sandbox root '{}': {e}",
                            root_for_closure.display()
                        ),
                    ));
                }
            };
            let rule = PathBeneath::new(fd, write_access);
            let ruleset = match ruleset.add_rule(rule) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[alms] Landlock: failed to add sandbox root rule: {e}");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("Landlock: failed to add sandbox root rule: {e}"),
                    ));
                }
            };

            // Allow read+execute access to system paths (missing paths are
            // skipped since not all distros have the same layout, but if a
            // present path fails to be added, that is a hard error — bash
            // won't work under the sandbox without /usr, /lib, etc.)
            let mut ruleset = ruleset;
            for sys_path in &sys_paths {
                if sys_path.exists()
                    && let Ok(fd) = PathFd::new(sys_path)
                {
                    let rule = PathBeneath::new(fd, read_access);
                    ruleset = match ruleset.add_rule(rule) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!(
                                "[alms] Landlock: failed to add system path rule for {}: {e}",
                                sys_path.display()
                            );
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                format!("Landlock: failed to add system path rule: {e}"),
                            ));
                        }
                    };
                }
            }

            match ruleset.restrict_self() {
                Ok(status) => {
                    if matches!(status.ruleset, RulesetStatus::NotEnforced) {
                        eprintln!("[alms] Landlock: ruleset was not enforced by kernel");
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "Landlock ruleset was not enforced by kernel",
                        ));
                    }
                }
                Err(e) => {
                    eprintln!("[alms] Landlock restrict_self failed: {e}");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("Landlock restrict_self failed: {e}"),
                    ));
                }
            }

            Ok(())
        });
    }

    debug!(
        sandbox_root = %canonical_root.display(),
        "Landlock filesystem sandbox configured for child process"
    );
}

/// Wrap a command string with a pwd marker so we can detect cwd changes.
///
/// The marker is appended after the command via `; echo <MARKER>; pwd`.
/// This way, even if the command fails, we still get the working directory.
///
/// The marker is supplied by the caller as a per-`ShellTool` random nonce
/// (see `ShellTool::new`). Using a per-instance nonce instead of a fixed
/// constant prevents a user script that intentionally or accidentally prints
/// the marker string from corrupting cwd tracking on a different agent
/// instance — each ShellTool only matches its own marker.
fn wrap_command_with_pwd_marker(command: &str, pwd_marker: &str) -> String {
    // Use a subshell to capture the command's output, then echo the marker
    // and the current working directory.
    format!("{command}; __alms_ec=$?; echo; echo '{pwd_marker}'; pwd; exit $__alms_ec")
}

/// Extract the cwd from command output by looking for the **last** PWD marker.
///
/// We match the last occurrence because command output could contain the
/// literal marker string. The real marker is always appended last by
/// `wrap_command_with_pwd_marker`.
///
/// Returns `Some(path_str)` if found, `None` if the marker is not present.
fn extract_cwd_from_output<'a>(stdout: &'a str, pwd_marker: &str) -> Option<&'a str> {
    let lines: Vec<&str> = stdout.lines().collect();
    // Search backwards for the last marker line
    let marker_idx = lines.iter().rposition(|line| line.trim() == pwd_marker)?;
    // The cwd is the next non-empty line after the marker
    for line in &lines[marker_idx + 1..] {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}

/// Byte-level variant of strip_pwd_marker used by the execution path.
///
/// Operates on the raw process-output bytes so lossy UTF-8 decoding is
/// deferred to the final boundary. The marker is ASCII so byte-level line
/// scanning is sufficient. Returns the bytes before the **last** marker
/// line; earlier occurrences (which could only originate from user output)
/// are preserved as user data, matching the semantics of the previous
/// string-level helper.
fn strip_pwd_marker_bytes(stdout: &[u8], pwd_marker: &str) -> Vec<u8> {
    let marker_bytes = pwd_marker.as_bytes();

    // Collect the byte ranges of each line. `end` excludes the trailing
    // '\n' (and the preceding '\r' if it forms a CRLF pair).
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    for (i, &b) in stdout.iter().enumerate() {
        if b == b'\n' {
            let mut end = i;
            if end > start && stdout[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push((start, end));
            start = i + 1;
        }
    }
    if start < stdout.len() {
        lines.push((start, stdout.len()));
    }

    // Trim ASCII whitespace from each line before comparing to the marker.
    let is_marker = |range: &(usize, usize)| -> bool {
        let (s, e) = *range;
        let trimmed = trim_ascii_whitespace(&stdout[s..e]);
        trimmed == marker_bytes
    };

    let Some(marker_idx) = lines.iter().rposition(is_marker) else {
        // No marker — return the input bytes as-is.
        return stdout.to_vec();
    };

    // Take all bytes up to the beginning of the marker line.
    let mut end_byte = lines[marker_idx].0;

    // Strip exactly the one separator newline emitted by `wrap_command_with_pwd_marker`'s
    // bare `echo;` between the user's command and the marker. That separator is a single
    // `\n` (or `\r\n` if the upstream stream happened to be CRLF) — nothing more.
    //
    // CRITICAL: do NOT walk back over arbitrary trailing newlines. The user's command may
    // legitimately end in `\n` (e.g. `printf '...'; echo` produces a trailing `\n` that
    // belongs to the user's output), and that byte must survive into the captured stdout
    // so byte-for-byte spill round-trips work. See #954.
    //
    // Known minor lossy edge case: if the user's output ends in a raw `\r` with no
    // following `\n` (i.e. wrapper bytes are `...a\r\n{MARKER}...`), the `\r` here is
    // indistinguishable from the leading half of a CRLF separator and gets stripped
    // alongside the `\n`. This is identical to the pre-#954 behavior and is realistic
    // only for very obscure binary-ish output; fixing it would require the wrapper to
    // emit a non-newline sentinel before the separator. Not worth the complexity today.
    if end_byte > 0 && stdout[end_byte - 1] == b'\n' {
        end_byte -= 1;
        if end_byte > 0 && stdout[end_byte - 1] == b'\r' {
            end_byte -= 1;
        }
    }

    stdout[..end_byte].to_vec()
}

/// Trim ASCII whitespace from both ends of a byte slice without allocating.
fn trim_ascii_whitespace(s: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = s.len();
    while start < end && (s[start] == b' ' || s[start] == b'\t') {
        start += 1;
    }
    while end > start && (s[end - 1] == b' ' || s[end - 1] == b'\t') {
        end -= 1;
    }
    &s[start..end]
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

    /// A canned marker used by the unit tests. Production code uses a
    /// per-instance random nonce generated in `ShellTool::new`.
    const TEST_MARKER: &str = "__ALMS_PWD_TEST_MARKER__";

    #[test]
    fn test_wrap_command_with_pwd_marker() {
        let wrapped = wrap_command_with_pwd_marker("ls -la", TEST_MARKER);
        assert!(wrapped.contains("ls -la"));
        assert!(wrapped.contains(TEST_MARKER));
        assert!(wrapped.contains("pwd"));
    }

    #[test]
    fn test_extract_cwd_from_output() {
        let output = format!("file1.txt\nfile2.txt\n\n{TEST_MARKER}\n/home/user/project\n");
        assert_eq!(
            extract_cwd_from_output(&output, TEST_MARKER),
            Some("/home/user/project")
        );
    }

    #[test]
    fn test_extract_cwd_no_marker() {
        let output = "file1.txt\nfile2.txt\n";
        assert_eq!(extract_cwd_from_output(output, TEST_MARKER), None);
    }

    #[test]
    fn test_strip_pwd_marker_bytes() {
        // Layout: <user-output>\n<separator-from-bare-echo>\n<marker>\n<pwd>\n
        // Here the user's output is "file1.txt\nfile2.txt\n" — the trailing \n
        // belongs to the user and must survive. Only the single separator \n
        // (and the marker line and pwd line) get stripped.
        let output = format!("file1.txt\nfile2.txt\n\n{TEST_MARKER}\n/home/user\n");
        let cleaned = strip_pwd_marker_bytes(output.as_bytes(), TEST_MARKER);
        let cleaned_str = String::from_utf8(cleaned).unwrap();
        assert_eq!(cleaned_str, "file1.txt\nfile2.txt\n");
        assert!(!cleaned_str.contains(TEST_MARKER));
        assert!(!cleaned_str.contains("/home/user"));
    }

    #[test]
    fn test_strip_pwd_marker_bytes_minimal_wrapper_shape() {
        // Tim's suggested contract test (PR #955): pure-byte assertion on the
        // canonical `wrap_command_with_pwd_marker` output shape, independent of
        // bash availability. Locks #954 across every platform.
        //
        // Layout: <user>\n<separator>\n<marker>\n<pwd>\n
        // Input : "aaa\n\n{MARKER}\n/home\n"
        // Output: "aaa\n"  (user's trailing \n survives; the bare-`echo` separator
        //                   and the marker/pwd lines are stripped — nothing more)
        let input = format!("aaa\n\n{TEST_MARKER}\n/home\n");
        let cleaned = strip_pwd_marker_bytes(input.as_bytes(), TEST_MARKER);
        assert_eq!(cleaned, b"aaa\n");
    }

    #[test]
    fn test_strip_pwd_marker_bytes_no_marker() {
        let output = b"line1\nline2\n";
        let cleaned = strip_pwd_marker_bytes(output, TEST_MARKER);
        // No marker, bytes are returned unchanged.
        assert_eq!(cleaned, output.to_vec());
    }

    #[test]
    fn test_strip_pwd_marker_bytes_preserves_crlf() {
        // Windows shells emit \r\n; we must NOT strip \r from user output.
        let output = b"line1\r\nline2\r\n\r\n__ALMS_PWD_TEST_MARKER__\r\n/home/user\r\n";
        let cleaned = strip_pwd_marker_bytes(output, TEST_MARKER);
        let cleaned_str = String::from_utf8(cleaned).unwrap();
        assert!(
            cleaned_str.contains("line1\r\n"),
            "CRLF on user output must survive: {cleaned_str:?}"
        );
        assert!(cleaned_str.contains("line2"));
        assert!(!cleaned_str.contains("__ALMS_PWD_TEST_MARKER__"));
        assert!(!cleaned_str.contains("/home/user"));
    }

    #[test]
    fn test_strip_pwd_marker_bytes_preserves_invalid_utf8() {
        // Binary-ish stdout must pass through the byte truncation step
        // unchanged until the lossy-decode boundary at the end.
        let mut output = Vec::new();
        output.extend_from_slice(b"before\n");
        output.push(0xFF); // invalid UTF-8 byte
        output.extend_from_slice(b"\n\n");
        output.extend_from_slice(TEST_MARKER.as_bytes());
        output.extend_from_slice(b"\n/home/user\n");

        let cleaned = strip_pwd_marker_bytes(&output, TEST_MARKER);
        assert!(cleaned.contains(&0xFF), "raw bytes must be preserved");
        assert!(
            !cleaned
                .windows(TEST_MARKER.len())
                .any(|w| w == TEST_MARKER.as_bytes())
        );
    }

    #[test]
    fn test_extract_cwd_matches_last_marker() {
        // If command output contains the marker string, we must match the LAST one
        let output = format!(
            "echo {TEST_MARKER}\n/fake/path\nreal output\n\n{TEST_MARKER}\n/home/user/real\n"
        );
        assert_eq!(
            extract_cwd_from_output(&output, TEST_MARKER),
            Some("/home/user/real")
        );
    }

    #[test]
    fn test_strip_pwd_marker_bytes_matches_last_marker() {
        // Only the last marker should be stripped; earlier occurrences are user data
        let output = format!(
            "echo {TEST_MARKER}\n/fake/path\nreal output\n\n{TEST_MARKER}\n/home/user/real\n"
        );
        let cleaned = strip_pwd_marker_bytes(output.as_bytes(), TEST_MARKER);
        let cleaned_str = String::from_utf8(cleaned).unwrap();
        assert!(
            cleaned_str.contains(TEST_MARKER),
            "first marker should be preserved as user data"
        );
        assert!(cleaned_str.contains("/fake/path"));
        assert!(cleaned_str.contains("real output"));
        assert!(
            !cleaned_str.contains("/home/user/real"),
            "real cwd line should be stripped"
        );
    }

    #[test]
    fn test_command_excerpt_short_returns_full() {
        assert_eq!(command_excerpt("ls -la"), "ls -la");
    }

    #[test]
    fn test_command_excerpt_long_is_truncated() {
        let long = "x".repeat(500);
        let excerpt = command_excerpt(&long);
        // For ASCII input we should be exactly 100 + ellipsis bytes.
        assert!(excerpt.len() < long.len());
        assert!(excerpt.ends_with('…'));
        assert_eq!(&excerpt[..100], &long[..100]);
    }

    #[test]
    fn test_build_command_wraps_with_bash() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let cmd = build_command("echo hello", cwd, None, true, TEST_MARKER).unwrap();
        // The command should be a bash process
        let inner = cmd.as_std();
        #[cfg(unix)]
        assert_eq!(inner.get_program(), "bash");
        #[cfg(windows)]
        assert!(
            inner.get_program().to_string_lossy().ends_with("bash.exe")
                || inner.get_program() == "bash"
        );
        let args: Vec<_> = inner.get_args().collect();
        assert_eq!(args[0], "-c");
        // The second arg should contain our command and the pwd marker
        let wrapped = args[1].to_str().unwrap();
        assert!(wrapped.contains("echo hello"));
        assert!(wrapped.contains(TEST_MARKER));
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
