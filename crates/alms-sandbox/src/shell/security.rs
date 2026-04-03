//! Security checks for the shell tool.
//!
//! Preserves all existing security features from `ShellExecTool`:
//! - Denied filenames (secrets.json, etc.)
//! - Secret env var filtering
//! - Platform-critical env var re-injection after `env_clear()`
//!
//! Adds:
//! - Pattern-based command denylist (dangerous destructive commands)

/// Filenames that must never be accessed by agent tools.
///
/// Single source of truth — imported by `builtin.rs` for fs_read/fs_write
/// path validation and used here for shell command/argv checks.
///
/// These are checked against the final component of resolved paths in fs_read,
/// fs_write, and against command strings in shell to prevent agents from
/// reading secrets or other sensitive files.
pub(crate) const DENIED_FILENAMES: &[&str] = &["secrets.json"];

/// Environment variable names that must never be injected into shell
/// child processes. Belt-and-suspenders protection: `env_clear()` already
/// strips the parent environment, but this ensures these names are also
/// filtered from the tool-call `env` parameter and `default_env`.
pub(crate) const SECRET_ENV_VARS: &[&str] = &[
    "OPENROUTER_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "TELEGRAM_BOT_TOKEN",
    "ALMS_MASTER_KEY",
    "ALMS_AUTH_TOKEN",
];

/// Dangerous command patterns that are denied outright.
///
/// These patterns match common destructive operations that an agent should
/// never execute without human intervention. The check is substring-based
/// against the full command string (case-insensitive).
const DENIED_COMMAND_PATTERNS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "mkfs.",
    "dd if=",
    "> /dev/sda",
    "chmod -R 777 /",
    ":(){ :|:& };:", // fork bomb
];

/// Check whether a command string references any denied filename.
///
/// This is a best-effort check: it catches obvious patterns like
/// `cat data/secrets.json` or `cat /abs/path/secrets.json` and also catches
/// `sh -c "cat secrets.json"` by scanning the command as a substring.
/// It cannot prevent all indirect access (e.g. `cat $(echo secrets.json)`,
/// base64 encoding, variable expansion). For true shell isolation, use a
/// restricted OS user or Landlock.
pub(crate) fn command_references_denied_file(command: &str) -> Option<&'static str> {
    let lower = command.to_ascii_lowercase();
    DENIED_FILENAMES
        .iter()
        .find(|denied| lower.contains(*denied))
        .copied()
}

/// Check whether a command matches any denied destructive pattern.
///
/// Returns `Some(pattern)` if the command is denied, `None` if allowed.
pub(crate) fn command_matches_denylist(command: &str) -> Option<&'static str> {
    let lower = command.to_ascii_lowercase();
    DENIED_COMMAND_PATTERNS
        .iter()
        .find(|pattern| lower.contains(*pattern))
        .copied()
}

/// Check whether an environment variable name is a known secret.
pub(crate) fn is_secret_env_var(name: &str) -> bool {
    SECRET_ENV_VARS.iter().any(|s| s.eq_ignore_ascii_case(name))
}

// NOTE: filter_env_vars helper removed — env filtering is done inline in
// exec.rs and mod.rs where the Command builder is available.

/// Returns a list of environment variable names that are critical for process
/// spawning on the current platform.
///
/// These variables are safe to inherit (they don't contain secrets) and are
/// re-injected after `env_clear()` so that child processes can run correctly.
/// On Windows, the absence of `SystemRoot`, `PATH`, `PATHEXT`, and `COMSPEC`
/// causes most executables to fail. On Unix, `PATH` is needed for command
/// resolution.
pub(crate) fn platform_critical_env_vars() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &["SystemRoot", "PATH", "PATHEXT", "COMSPEC"]
    }
    #[cfg(not(windows))]
    {
        &["PATH"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_references_denied_file() {
        assert_eq!(
            command_references_denied_file("cat data/secrets.json"),
            Some("secrets.json")
        );
        assert_eq!(
            command_references_denied_file("cat /abs/path/secrets.json"),
            Some("secrets.json")
        );
        assert_eq!(
            command_references_denied_file("cat Secrets.JSON"),
            Some("secrets.json")
        );
        assert_eq!(command_references_denied_file("ls -la"), None);
        assert_eq!(command_references_denied_file("cat data.json"), None);
    }

    #[test]
    fn test_command_matches_denylist() {
        assert!(command_matches_denylist("rm -rf /").is_some());
        assert!(command_matches_denylist("rm -rf /*").is_some());
        assert!(command_matches_denylist("sudo mkfs.ext4 /dev/sda1").is_some());
        assert!(command_matches_denylist("dd if=/dev/zero of=/dev/sda").is_some());
        assert!(command_matches_denylist(":(){ :|:& };:").is_some());
        assert!(command_matches_denylist("ls -la").is_none());
        assert!(command_matches_denylist("rm -rf ./build").is_none());
    }

    #[test]
    fn test_is_secret_env_var() {
        assert!(is_secret_env_var("OPENAI_API_KEY"));
        assert!(is_secret_env_var("openai_api_key")); // case-insensitive
        assert!(is_secret_env_var("ALMS_AUTH_TOKEN"));
        assert!(!is_secret_env_var("PATH"));
        assert!(!is_secret_env_var("ALMS_DATA_DIR"));
    }

    #[test]
    fn test_platform_critical_env_vars_not_empty() {
        let vars = platform_critical_env_vars();
        assert!(!vars.is_empty());
        assert!(vars.contains(&"PATH"));
    }

    #[cfg(windows)]
    #[test]
    fn test_platform_critical_env_vars_windows() {
        let vars = platform_critical_env_vars();
        for expected in &["SystemRoot", "PATH", "PATHEXT", "COMSPEC"] {
            assert!(vars.contains(expected));
        }
    }
}
