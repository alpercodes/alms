//! Permission-based allow/deny list for shell commands.
//!
//! Evaluates regex patterns against command strings before execution.
//! This acts as a policy gate integrated into the shell execution pipeline.
//!
//! Evaluation order (inside `check_command`):
//! 1. If any `denied_commands` pattern matches, the command is blocked.
//! 2. If `allowed_commands` is non-empty, only commands matching at least
//!    one pattern are permitted (allowlist mode).
//! 3. If `allowed_commands` is empty, all non-denied commands are allowed
//!    (denylist-only mode).
//!
//! The `classifier_overrides` list is checked separately via
//! [`CompiledPermissions::matching_classifier_override`] — operators use
//! it to exempt specific commands from the built-in risk classifier.
//! Overrides do **not** bypass the deny list.

use crate::SandboxError;
use crate::error::SandboxResult;
use alms_core::config::ShellPermissions;
use regex::Regex;
use tracing::{debug, warn};

/// Compiled permission rules for efficient repeated matching.
///
/// Constructed once from [`ShellPermissions`] config and reused across
/// command invocations. Invalid regex patterns are logged and skipped
/// rather than causing a hard failure (fail-open for individual patterns,
/// fail-closed for the overall policy).
#[derive(Debug, Clone)]
pub struct CompiledPermissions {
    /// Compiled allow patterns. Empty means "allow everything not denied".
    allowed: Vec<CompiledPattern>,
    /// Compiled deny patterns.
    denied: Vec<CompiledPattern>,
    /// Compiled classifier-override patterns. A command matching one of these
    /// bypasses the built-in risk classifier (but not the deny list).
    classifier_overrides: Vec<CompiledPattern>,
}

/// A single compiled regex pattern with its original source for error messages.
#[derive(Debug, Clone)]
struct CompiledPattern {
    /// The original pattern string (for error messages).
    source: String,
    /// The compiled regex.
    regex: Regex,
}

impl CompiledPermissions {
    /// Compile permission rules from config.
    ///
    /// Invalid regex patterns are logged as warnings and skipped. This
    /// prevents a typo in one pattern from disabling the entire permission
    /// system. The rationale: it is safer to skip a broken deny pattern
    /// (allowing slightly more) than to hard-fail the entire shell tool.
    pub fn compile(permissions: &ShellPermissions) -> Self {
        let allowed = compile_patterns(&permissions.allowed_commands, "allowed");
        let denied = compile_patterns(&permissions.denied_commands, "denied");
        let classifier_overrides =
            compile_patterns(&permissions.classifier_overrides, "classifier_overrides");

        if !allowed.is_empty() || !denied.is_empty() || !classifier_overrides.is_empty() {
            debug!(
                allowed_count = allowed.len(),
                denied_count = denied.len(),
                classifier_override_count = classifier_overrides.len(),
                "Shell permissions compiled"
            );
        }

        Self {
            allowed,
            denied,
            classifier_overrides,
        }
    }

    /// Check whether a command is permitted by the configured rules.
    ///
    /// Returns `Ok(())` if the command is allowed, or `Err(SandboxError::SandboxViolation)`
    /// with a descriptive message if the command is blocked.
    ///
    /// This does **not** consult `classifier_overrides` — those are applied
    /// separately by the caller, *between* the allow/deny check and the risk
    /// classifier (see `ShellTool::execute`).
    pub fn check_command(&self, command: &str) -> SandboxResult<()> {
        // If no allow/deny patterns are configured, everything is allowed.
        // `classifier_overrides` is intentionally not considered here — it
        // controls classifier behaviour, not whether the command is admitted.
        if self.allowed.is_empty() && self.denied.is_empty() {
            return Ok(());
        }

        // Step 1: Check deny patterns first. Deny always wins.
        for pattern in &self.denied {
            if pattern.regex.is_match(command) {
                // Log the specific pattern for operators (visible in server logs)
                // but return a generic message to the agent to avoid leaking
                // regex patterns that could aid prompt-injection evasion.
                debug!(
                    command = %command,
                    pattern = %pattern.source,
                    "Shell command denied by permission rule"
                );
                return Err(SandboxError::SandboxViolation(
                    "Command blocked by security policy".to_string(),
                ));
            }
        }

        // Step 2: If allow patterns exist, command must match at least one.
        if !self.allowed.is_empty() {
            let matches_allow = self.allowed.iter().any(|p| p.regex.is_match(command));
            if !matches_allow {
                debug!(
                    command = %command,
                    "Shell command not in allowlist"
                );
                return Err(SandboxError::SandboxViolation(
                    "Command not permitted: does not match any allowed command pattern".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Returns true if no allow/deny patterns are configured (no-op mode).
    ///
    /// Classifier overrides are intentionally excluded — they control
    /// classifier behaviour, not whether any given command is admitted. A
    /// `CompiledPermissions` with only overrides is still "empty" from the
    /// `check_command` perspective (all commands pass the admission gate).
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty() && self.denied.is_empty()
    }

    /// Returns `true` if any `classifier_overrides` pattern matches the command.
    ///
    /// The override list is opt-in, operator-only, and never model-visible.
    /// A match means "the operator has explicitly declared this command safe
    /// enough to skip the built-in risk classifier". The deny list still
    /// applies to overridden commands; overrides do **not** weaken any other
    /// part of the defence chain.
    pub fn classifier_override_matches(&self, command: &str) -> bool {
        self.classifier_overrides
            .iter()
            .any(|p| p.regex.is_match(command))
    }

    /// Returns the source pattern of the first classifier override that matches.
    /// Intended for structured logging of override hits.
    pub fn matching_classifier_override(&self, command: &str) -> Option<&str> {
        self.classifier_overrides
            .iter()
            .find(|p| p.regex.is_match(command))
            .map(|p| p.source.as_str())
    }
}

/// Compile a list of regex pattern strings, skipping invalid ones with warnings.
fn compile_patterns(patterns: &[String], label: &str) -> Vec<CompiledPattern> {
    patterns
        .iter()
        .filter_map(|source| {
            // Skip empty patterns silently
            if source.trim().is_empty() {
                return None;
            }
            match Regex::new(source) {
                Ok(regex) => Some(CompiledPattern {
                    source: source.clone(),
                    regex,
                }),
                Err(e) => {
                    warn!(
                        pattern = %source,
                        error = %e,
                        list = label,
                        "Invalid regex in shell permissions, skipping"
                    );
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create permissions from string slices.
    fn perms(allowed: &[&str], denied: &[&str]) -> CompiledPermissions {
        let config = ShellPermissions {
            allowed_commands: allowed.iter().map(|s| s.to_string()).collect(),
            denied_commands: denied.iter().map(|s| s.to_string()).collect(),
            classifier_overrides: vec![],
        };
        CompiledPermissions::compile(&config)
    }

    /// Helper to create permissions with classifier overrides included.
    fn perms_with_overrides(
        allowed: &[&str],
        denied: &[&str],
        overrides: &[&str],
    ) -> CompiledPermissions {
        let config = ShellPermissions {
            allowed_commands: allowed.iter().map(|s| s.to_string()).collect(),
            denied_commands: denied.iter().map(|s| s.to_string()).collect(),
            classifier_overrides: overrides.iter().map(|s| s.to_string()).collect(),
        };
        CompiledPermissions::compile(&config)
    }

    // ── No patterns configured (no-op) ──────────────────────────────────

    #[test]
    fn empty_permissions_allow_everything() {
        let p = perms(&[], &[]);
        assert!(p.is_empty());
        assert!(p.check_command("rm -rf /").is_ok());
        assert!(p.check_command("ls -la").is_ok());
        assert!(p.check_command("echo hello").is_ok());
    }

    // ── Deny-only mode ──────────────────────────────────────────────────

    #[test]
    fn deny_pattern_blocks_matching_command() {
        let p = perms(&[], &[r"^rm\s+-rf\s+/"]);
        assert!(p.check_command("rm -rf /").is_err());
        assert!(p.check_command("rm -rf /home").is_err());
        assert!(p.check_command("ls -la").is_ok());
    }

    #[test]
    fn deny_pattern_case_sensitive_by_default() {
        let p = perms(&[], &[r"^DROP\s+TABLE"]);
        assert!(p.check_command("DROP TABLE users").is_err());
        // Case-sensitive: lowercase should not match
        assert!(p.check_command("drop table users").is_ok());
    }

    #[test]
    fn deny_pattern_can_use_case_insensitive_flag() {
        let p = perms(&[], &[r"(?i)^drop\s+table"]);
        assert!(p.check_command("DROP TABLE users").is_err());
        assert!(p.check_command("drop table users").is_err());
        assert!(p.check_command("Drop Table users").is_err());
    }

    #[test]
    fn multiple_deny_patterns_any_match_blocks() {
        let p = perms(&[], &[r"^rm\s+-rf", r"mkfs\.", r"dd\s+if="]);
        assert!(p.check_command("rm -rf /").is_err());
        assert!(p.check_command("sudo mkfs.ext4 /dev/sda1").is_err());
        assert!(p.check_command("dd if=/dev/zero of=/dev/sda").is_err());
        assert!(p.check_command("echo hello").is_ok());
    }

    // ── Allow-only mode ─────────────────────────────────────────────────

    #[test]
    fn allow_pattern_permits_matching_command() {
        let p = perms(&[r"^(ls|cat|echo)\b"], &[]);
        assert!(p.check_command("ls -la").is_ok());
        assert!(p.check_command("cat file.txt").is_ok());
        assert!(p.check_command("echo hello").is_ok());
        assert!(p.check_command("rm -rf /").is_err());
    }

    #[test]
    fn allow_pattern_rejects_non_matching_command() {
        let p = perms(&[r"^echo\b"], &[]);
        assert!(p.check_command("echo hello").is_ok());
        assert!(p.check_command("ls -la").is_err());
    }

    #[test]
    fn multiple_allow_patterns_any_match_permits() {
        let p = perms(&[r"^ls\b", r"^cat\b", r"^echo\b"], &[]);
        assert!(p.check_command("ls").is_ok());
        assert!(p.check_command("cat foo").is_ok());
        assert!(p.check_command("echo bar").is_ok());
        assert!(p.check_command("rm file").is_err());
    }

    // ── Combined allow + deny ───────────────────────────────────────────

    #[test]
    fn deny_takes_precedence_over_allow() {
        // Allow git commands, but deny force push
        let p = perms(&[r"^git\b"], &[r"git\s+push\s+.*--force"]);
        assert!(p.check_command("git status").is_ok());
        assert!(p.check_command("git commit -m 'test'").is_ok());
        assert!(p.check_command("git push origin main").is_ok());
        assert!(p.check_command("git push --force origin main").is_err());
        assert!(p.check_command("git push origin main --force").is_err());
    }

    #[test]
    fn deny_blocks_even_if_allowed() {
        let p = perms(&[r".*"], &[r"rm\s+-rf"]);
        // ".*" allows everything, but deny still blocks rm -rf
        assert!(p.check_command("ls").is_ok());
        assert!(p.check_command("rm -rf /").is_err());
    }

    // ── Edge cases ──────────────────────────────────────────────────────

    #[test]
    fn empty_command_string() {
        let p = perms(&[r"^echo\b"], &[]);
        assert!(p.check_command("").is_err());
    }

    #[test]
    fn whitespace_only_patterns_are_skipped() {
        let p = perms(&["  ", "\t"], &["", " "]);
        assert!(p.is_empty());
        assert!(p.check_command("ls").is_ok());
    }

    #[test]
    fn invalid_regex_patterns_are_skipped() {
        // Invalid regex: unclosed group
        let p = perms(&[r"^echo\b", r"(unclosed"], &[r"[invalid", r"^rm\b"]);
        // Valid patterns should still work
        assert!(p.check_command("echo hello").is_ok());
        assert!(p.check_command("rm -rf /").is_err());
        // "ls" should be blocked by allowlist (only echo is allowed)
        assert!(p.check_command("ls").is_err());
    }

    #[test]
    fn pattern_matches_substring_not_just_full_string() {
        // Patterns without anchors match substrings
        let p = perms(&[], &[r"secrets"]);
        assert!(p.check_command("cat data/secrets.json").is_err());
        assert!(p.check_command("echo my-secrets-file").is_err());
        assert!(p.check_command("echo hello").is_ok());
    }

    #[test]
    fn anchored_pattern_only_matches_start() {
        let p = perms(&[], &[r"^rm\b"]);
        assert!(p.check_command("rm file.txt").is_err());
        // "echo rm" should NOT match because the pattern is anchored to start
        assert!(p.check_command("echo rm file.txt").is_ok());
    }

    // ── Error messages ──────────────────────────────────────────────────

    #[test]
    fn deny_error_is_generic_no_pattern_leak() {
        let p = perms(&[], &[r"^rm\s+-rf"]);
        let err = p.check_command("rm -rf /").unwrap_err();
        let msg = err.to_string();
        // Error message should be generic to avoid leaking regex patterns
        // that could aid prompt-injection evasion strategies.
        assert!(msg.contains("Command blocked by security policy"));
        // Must NOT contain the regex pattern
        assert!(
            !msg.contains("^rm"),
            "error message must not leak the deny pattern"
        );
    }

    #[test]
    fn allow_error_explains_not_in_allowlist() {
        let p = perms(&[r"^echo\b"], &[]);
        let err = p.check_command("ls").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does not match any allowed command pattern"));
    }

    // ── ShellPermissions merge ──────────────────────────────────────────

    #[test]
    fn merge_combines_deny_lists() {
        let global = ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec!["^rm\\b".to_string()],
            classifier_overrides: vec![],
        };
        let agent = ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec!["^mkfs".to_string()],
            classifier_overrides: vec![],
        };
        let merged = global.merge_with(&agent);
        assert_eq!(merged.denied_commands.len(), 2);
        assert!(merged.denied_commands.contains(&"^rm\\b".to_string()));
        assert!(merged.denied_commands.contains(&"^mkfs".to_string()));
    }

    #[test]
    fn merge_agent_allow_replaces_global_when_nonempty() {
        let global = ShellPermissions {
            allowed_commands: vec!["^ls\\b".to_string(), "^cat\\b".to_string()],
            denied_commands: vec![],
            classifier_overrides: vec![],
        };
        let agent = ShellPermissions {
            allowed_commands: vec!["^git\\b".to_string()],
            denied_commands: vec![],
            classifier_overrides: vec![],
        };
        let merged = global.merge_with(&agent);
        assert_eq!(merged.allowed_commands, vec!["^git\\b".to_string()]);
    }

    #[test]
    fn merge_agent_empty_allow_inherits_global() {
        let global = ShellPermissions {
            allowed_commands: vec!["^ls\\b".to_string()],
            denied_commands: vec![],
            classifier_overrides: vec![],
        };
        let agent = ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec!["^rm\\b".to_string()],
            classifier_overrides: vec![],
        };
        let merged = global.merge_with(&agent);
        assert_eq!(merged.allowed_commands, vec!["^ls\\b".to_string()]);
        assert_eq!(merged.denied_commands, vec!["^rm\\b".to_string()]);
    }

    #[test]
    fn merge_both_empty() {
        let global = ShellPermissions::default();
        let agent = ShellPermissions::default();
        let merged = global.merge_with(&agent);
        assert!(merged.is_empty());
    }

    // ── Compiled from default config ────────────────────────────────────

    #[test]
    fn default_permissions_are_empty() {
        let p = CompiledPermissions::compile(&ShellPermissions::default());
        assert!(p.is_empty());
        assert!(p.check_command("anything").is_ok());
        assert!(!p.classifier_override_matches("anything"));
    }

    // ── Classifier overrides (issue #745) ───────────────────

    #[test]
    fn classifier_overrides_are_compiled() {
        let p = perms_with_overrides(&[], &[], &[r"^sudo apt-get update$"]);
        assert!(p.classifier_override_matches("sudo apt-get update"));
        assert!(!p.classifier_override_matches("sudo rm -rf /"));
    }

    #[test]
    fn classifier_overrides_do_not_affect_check_command() {
        // An override list must NOT substitute for an allowlist match.
        // Commands that would otherwise fail the allowlist still fail.
        let p = perms_with_overrides(&[r"^echo\b"], &[], &[r"^sudo\b"]);
        assert!(
            p.check_command("sudo rm -rf /").is_err(),
            "override must not satisfy the allowlist"
        );
        assert!(p.check_command("echo hi").is_ok());
    }

    #[test]
    fn classifier_overrides_respect_deny_list() {
        // An override is not a super-power — the deny list still wins.
        let p = perms_with_overrides(&[r".*"], &[r"rm\s+-rf"], &[r"rm\s+-rf"]);
        assert!(p.classifier_override_matches("rm -rf /"));
        // check_command still rejects it because deny wins.
        assert!(p.check_command("rm -rf /").is_err());
    }

    #[test]
    fn is_empty_ignores_classifier_overrides() {
        // A permissions set with only overrides is still "empty" from the
        // admission-gate perspective — check_command allows everything.
        let p = perms_with_overrides(&[], &[], &[r"^anything$"]);
        assert!(p.is_empty());
        assert!(p.check_command("arbitrary").is_ok());
    }

    #[test]
    fn matching_classifier_override_returns_source_pattern() {
        let p = perms_with_overrides(&[], &[], &[r"^sudo apt-get\b", r"^docker\b"]);
        assert_eq!(
            p.matching_classifier_override("sudo apt-get update"),
            Some("^sudo apt-get\\b")
        );
        assert_eq!(p.matching_classifier_override("ls"), None);
    }

    #[test]
    fn invalid_override_regex_is_skipped() {
        let p = perms_with_overrides(&[], &[], &[r"(unclosed", r"^echo\b"]);
        // The valid pattern still works
        assert!(p.classifier_override_matches("echo hi"));
        // The invalid pattern was silently dropped — no match on arbitrary input
        assert!(!p.classifier_override_matches("(unclosed anything"));
    }

    #[test]
    fn merge_classifier_overrides_union() {
        // Overrides are operator-only today; using union semantics matches
        // `denied_commands`. See `ShellPermissions::merge_with` for the
        // threat-model caveat if per-agent config ever becomes less trusted.
        let global = ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec![],
            classifier_overrides: vec!["^sudo apt-get\\b".to_string()],
        };
        let agent = ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec![],
            classifier_overrides: vec!["^docker\\b".to_string()],
        };
        let merged = global.merge_with(&agent);
        assert_eq!(merged.classifier_overrides.len(), 2);
        assert!(
            merged
                .classifier_overrides
                .contains(&"^sudo apt-get\\b".to_string())
        );
        assert!(
            merged
                .classifier_overrides
                .contains(&"^docker\\b".to_string())
        );
    }
}
