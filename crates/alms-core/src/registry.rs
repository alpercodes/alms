//! Agent registry types — persistent named agents.
//!
//! An `AgentRecord` represents a named, persistent agent registered in the system.
//! Each agent has a unique slug name, optional per-agent config overrides, and
//! its own workspace/sessions/jobs.

use crate::{AgentId, AlmsError, AlmsResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A persistent agent registered in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: AgentId,
    pub name: String,
    pub description: String,
    /// Per-agent model override (None = use server default).
    pub model: Option<String>,
    /// Per-agent posture override (None = use server default).
    pub posture: Option<String>,
    /// Per-agent LLM provider override (None = use server default).
    pub provider: Option<String>,
    /// Per-agent Telegram bot token (None = no dedicated Telegram bot).
    ///
    /// When set, the gateway spawns a dedicated polling loop for this agent's
    /// bot and routes all messages from that bot to this agent.
    /// Never serialized in list responses for security -- use `has_telegram`
    /// in the API response instead.
    #[serde(skip_serializing)]
    pub telegram_token: Option<String>,
    /// Per-agent Anthropic extended-thinking budget override.
    ///
    /// `None` = inherit the server default from `[llm.anthropic]`.
    /// `Some(0)` = explicitly disable extended thinking for this agent
    /// even when the server default enables it.
    /// `Some(n > 0)` = use exactly `n` tokens.
    ///
    /// Only applies when the resolved provider maps to the Anthropic wire
    /// protocol; silently ignored for other providers.
    pub thinking_budget_tokens: Option<u32>,
    /// Per-agent OpenAI-compat reasoning-effort override (#768).
    ///
    /// `None` = inherit the server default from `[llm.openai]`.
    /// `Some(effort)` = use exactly this effort level for this agent.
    ///
    /// Only applies when the resolved provider maps to the OpenAI-compatible
    /// wire protocol and the model is a reasoning model (o-series, GPT-5,
    /// xAI Grok reasoning variants). DeepSeek R1 accepts no request-side
    /// param — reasoning fires automatically on `deepseek-reasoner`. For
    /// non-reasoning models (gpt-4o, etc.) the value is silently stripped
    /// from the request body because those models return 400 on unknown
    /// params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    /// Per-agent Gemini extended-thinking budget override (#794).
    ///
    /// `None` = inherit the server default from `[llm.gemini].thinking_budget`.
    /// `Some(0)` = explicitly disable extended thinking for this agent
    /// even when the server default enables it.
    /// `Some(n > 0)` = use exactly `n` tokens.
    ///
    /// Only applies when the resolved provider maps to the Gemini wire
    /// protocol; silently ignored for other providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_thinking_budget: Option<u32>,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}

/// Request body for creating a new agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub model: Option<String>,
    pub posture: Option<String>,
    pub provider: Option<String>,
    /// Per-agent Telegram bot token. Validated via `getMe` on gateway startup,
    /// not at persist time.
    pub telegram_token: Option<String>,
    /// Per-agent Anthropic extended-thinking budget override.
    /// See [`AgentRecord::thinking_budget_tokens`] for semantics.
    #[serde(default)]
    pub thinking_budget_tokens: Option<u32>,
    /// Per-agent OpenAI-compat reasoning-effort override (#768).
    /// See [`AgentRecord::reasoning_effort`] for semantics.
    #[serde(default)]
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    /// Per-agent Gemini extended-thinking budget override (#794).
    /// See [`AgentRecord::gemini_thinking_budget`] for semantics.
    #[serde(default)]
    pub gemini_thinking_budget: Option<u32>,
    #[serde(default)]
    pub is_default: Option<bool>,
}

/// Request body for updating an existing agent.
///
/// All fields are optional — only non-`None` fields are applied.
/// To clear an override, pass an empty string (handler treats `""` as `None`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAgentRequest {
    pub description: Option<String>,
    pub model: Option<String>,
    pub posture: Option<String>,
    pub provider: Option<String>,
    /// Per-agent Telegram bot token. Empty string = remove token.
    pub telegram_token: Option<String>,
    /// Per-agent Anthropic extended-thinking budget override. A value of
    /// `0` explicitly disables extended thinking for this agent even when
    /// the server default enables it. Omitting the field leaves the
    /// existing value unchanged.
    pub thinking_budget_tokens: Option<u32>,
    /// Per-agent OpenAI-compat reasoning-effort override (#768). Omitting
    /// the field leaves the existing value unchanged. There is no
    /// sentinel value to clear the override back to "inherit server
    /// default" today — mirrors the `thinking_budget_tokens` PATCH
    /// semantics.
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    /// Per-agent Gemini extended-thinking budget override (#794). A value
    /// of `0` explicitly disables extended thinking for this agent even
    /// when the server default enables it. Omitting the field leaves the
    /// existing value unchanged. No sentinel to clear the override back
    /// to "inherit server default" — mirrors `thinking_budget_tokens`
    /// PATCH semantics.
    pub gemini_thinking_budget: Option<u32>,
}

/// Validate an agent name slug.
///
/// Rules: 1–64 chars, lowercase alphanumeric + hyphens, no leading/trailing hyphens.
/// Must not be a valid UUID (would collide with UUID-first resolve_agent lookup).
pub fn validate_agent_name(name: &str) -> AlmsResult<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(AlmsError::InvalidConfig(format!(
            "agent name must be 1–64 characters, got {} characters",
            name.len()
        )));
    }

    let bytes = name.as_bytes();

    // No leading or trailing hyphens
    if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return Err(AlmsError::InvalidConfig(format!(
            "agent name must not start or end with a hyphen, got '{name}'"
        )));
    }

    // Only lowercase alphanumeric + hyphens
    for ch in name.chars() {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '-' {
            return Err(AlmsError::InvalidConfig(format!(
                "agent name must contain only lowercase letters, digits, and hyphens, got '{ch}' in '{name}'"
            )));
        }
    }

    // Reserved names that collide with API sub-route segments or internal prefixes
    const RESERVED_NAMES: &[&str] = &["default", "dm", "workspace"];
    if RESERVED_NAMES.contains(&name) {
        return Err(AlmsError::InvalidConfig(format!(
            "agent name '{name}' is reserved"
        )));
    }

    // Reject names that parse as UUIDs — resolve_agent uses UUID-first lookup,
    // so a UUID-shaped name would be unreachable by name.
    if uuid::Uuid::parse_str(name).is_ok() {
        return Err(AlmsError::InvalidConfig(format!(
            "agent name '{name}' looks like a UUID (conflicts with ID-based lookup)"
        )));
    }

    Ok(())
}

/// Workspace file names created for every new agent.
pub const WORKSPACE_FILENAMES: &[&str] = &["personality.md", "goals.md", "memories.md", "user.md"];

/// Migrate workspace directories from UUID-based to name-based paths.
///
/// For each `(uuid, name)` pair, renames `{workspace_dir}/{uuid}/` to
/// `{workspace_dir}/{name}/` if the UUID directory exists and the name
/// directory does not. Idempotent and non-destructive — skips if both
/// exist or neither exists.
pub fn migrate_workspace_dirs(
    workspace_dir: &Path,
    agents: &[(uuid::Uuid, String)],
) -> std::io::Result<usize> {
    let mut migrated = 0;
    for (uuid, name) in agents {
        let uuid_dir = workspace_dir.join(uuid.to_string());
        let name_dir = workspace_dir.join(name);
        if uuid_dir.is_dir() && !name_dir.exists() {
            std::fs::rename(&uuid_dir, &name_dir)?;
            migrated += 1;
            tracing::debug!(
                agent_name = %name,
                from = %uuid_dir.display(),
                to = %name_dir.display(),
                "Migrated workspace directory from UUID to name-based path"
            );
        } else if uuid_dir.is_dir() && name_dir.exists() {
            tracing::warn!(
                agent_name = %name,
                uuid_dir = %uuid_dir.display(),
                name_dir = %name_dir.display(),
                "Both UUID and name workspace directories exist — skipping migration; \
                 orphaned UUID directory may need manual cleanup"
            );
        }
    }
    Ok(migrated)
}

/// Create empty workspace files in a directory.
///
/// Skips files that already exist so the function is idempotent.
/// The directory is created if it doesn't exist.
pub fn init_workspace_files(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for filename in WORKSPACE_FILENAMES {
        let path = dir.join(filename);
        if !path.exists() {
            std::fs::write(&path, "")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_names() {
        assert!(validate_agent_name("atlas").is_ok());
        assert!(validate_agent_name("my-agent-1").is_ok());
        assert!(validate_agent_name("a").is_ok());
        assert!(validate_agent_name("agent-v2").is_ok());
        assert!(validate_agent_name("a1b2c3").is_ok());
    }

    #[test]
    fn test_reserved_names() {
        let err = validate_agent_name("default").unwrap_err();
        assert!(err.to_string().contains("reserved"));
        let err = validate_agent_name("dm").unwrap_err();
        assert!(err.to_string().contains("reserved"));
        let err = validate_agent_name("workspace").unwrap_err();
        assert!(err.to_string().contains("reserved"));
    }

    #[test]
    fn test_invalid_empty() {
        assert!(validate_agent_name("").is_err());
    }

    #[test]
    fn test_invalid_too_long() {
        let name = "a".repeat(65);
        assert!(validate_agent_name(&name).is_err());
    }

    #[test]
    fn test_invalid_uppercase() {
        assert!(validate_agent_name("MyAgent").is_err());
    }

    #[test]
    fn test_invalid_underscore() {
        assert!(validate_agent_name("my_agent").is_err());
    }

    #[test]
    fn test_invalid_leading_hyphen() {
        assert!(validate_agent_name("-agent").is_err());
    }

    #[test]
    fn test_invalid_trailing_hyphen() {
        assert!(validate_agent_name("agent-").is_err());
    }

    #[test]
    fn test_invalid_spaces() {
        assert!(validate_agent_name("my agent").is_err());
    }

    #[test]
    fn test_max_length_ok() {
        let name = "a".repeat(64);
        assert!(validate_agent_name(&name).is_ok());
    }

    #[test]
    fn test_uuid_shaped_name_rejected() {
        let err = validate_agent_name("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap_err();
        assert!(err.to_string().contains("UUID"));
        // Also test a v4-style UUID
        let err = validate_agent_name("550e8400-e29b-41d4-a716-446655440000").unwrap_err();
        assert!(err.to_string().contains("UUID"));
    }

    #[test]
    fn test_migrate_workspace_dirs_renames_uuid_to_name() {
        let tmp = std::env::temp_dir().join(format!("alms-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let uuid = uuid::Uuid::new_v4();
        let uuid_dir = tmp.join(uuid.to_string());
        init_workspace_files(&uuid_dir).unwrap();
        std::fs::write(uuid_dir.join("personality.md"), "hello").unwrap();

        let migrated = migrate_workspace_dirs(&tmp, &[(uuid, "atlas".to_string())]).unwrap();
        assert_eq!(migrated, 1);
        assert!(!uuid_dir.exists(), "UUID dir should be gone");
        assert!(
            tmp.join("atlas").join("personality.md").exists(),
            "name dir should have files"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.join("atlas").join("personality.md")).unwrap(),
            "hello"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_workspace_dirs_skips_when_name_exists() {
        let tmp = std::env::temp_dir().join(format!("alms-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let uuid = uuid::Uuid::new_v4();
        let uuid_dir = tmp.join(uuid.to_string());
        let name_dir = tmp.join("atlas");
        init_workspace_files(&uuid_dir).unwrap();
        init_workspace_files(&name_dir).unwrap();

        // Both exist — migration should skip (no overwrite)
        let migrated = migrate_workspace_dirs(&tmp, &[(uuid, "atlas".to_string())]).unwrap();
        assert_eq!(migrated, 0);
        assert!(uuid_dir.exists(), "UUID dir should still exist");
        assert!(name_dir.exists(), "name dir should still exist");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_workspace_dirs_idempotent() {
        let tmp = std::env::temp_dir().join(format!("alms-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let uuid = uuid::Uuid::new_v4();
        let uuid_dir = tmp.join(uuid.to_string());
        init_workspace_files(&uuid_dir).unwrap();

        let agents = vec![(uuid, "atlas".to_string())];
        let m1 = migrate_workspace_dirs(&tmp, &agents).unwrap();
        assert_eq!(m1, 1);
        // Second call — UUID dir is gone, name dir exists, should return 0
        let m2 = migrate_workspace_dirs(&tmp, &agents).unwrap();
        assert_eq!(m2, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_init_workspace_files_creates_all() {
        let tmp = std::env::temp_dir().join(format!("alms-test-{}", uuid::Uuid::new_v4()));
        let dir = tmp.join("my-agent");
        init_workspace_files(&dir).unwrap();
        for filename in WORKSPACE_FILENAMES {
            assert!(dir.join(filename).exists(), "{filename} should exist");
        }
        // Idempotent — writing content then calling again should not overwrite
        std::fs::write(dir.join("goals.md"), "Keep this").unwrap();
        init_workspace_files(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("goals.md")).unwrap(),
            "Keep this"
        );
        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
