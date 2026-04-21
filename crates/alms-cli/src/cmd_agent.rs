use alms_core::{AgentId, AgentRecord, validate_agent_name};
use alms_session::SqliteStore;
use clap::Subcommand;

use crate::helpers::{fmt_time, resolve_agent, short_id};

#[derive(Subcommand, Debug)]
pub(crate) enum AgentCommands {
    /// List all registered agents
    List,
    /// Create a new agent
    Create {
        /// Agent name slug (lowercase, hyphens, 1-64 chars)
        name: String,
        /// Agent description
        #[arg(long)]
        description: Option<String>,
        /// Model override
        #[arg(long)]
        model: Option<String>,
        /// Posture override ("guarded", "full_control", or "autonomous")
        #[arg(long)]
        posture: Option<String>,
        /// LLM provider override ("openai", "anthropic", "openrouter")
        #[arg(long)]
        provider: Option<String>,
        /// Anthropic extended-thinking budget in tokens (Claude 4.x).
        /// `0` explicitly disables thinking for this agent even when the
        /// server default enables it. Omit the flag to inherit the server
        /// default (three-layer precedence: per-run > per-agent > server).
        #[arg(long)]
        thinking_budget_tokens: Option<u32>,
        /// Set as the default agent
        #[arg(long)]
        default: bool,
    },
    /// Show details of a specific agent
    Show {
        /// Agent name slug or UUID
        name_or_id: String,
    },
    /// Delete an agent
    Delete {
        /// Agent name slug or UUID
        name_or_id: String,
        /// Force delete even if the agent is the default
        #[arg(long)]
        force: bool,
    },
    /// Set an agent as the default
    SetDefault {
        /// Agent name slug or UUID
        name_or_id: String,
    },
    /// Update an agent's configuration
    Config {
        /// Agent name slug or UUID
        name_or_id: String,
        /// Model override (empty string to clear)
        #[arg(long)]
        model: Option<String>,
        /// Posture override (empty string to clear)
        #[arg(long)]
        posture: Option<String>,
        /// LLM provider override (empty string to clear)
        #[arg(long)]
        provider: Option<String>,
        /// Description
        #[arg(long)]
        description: Option<String>,
        /// Anthropic extended-thinking budget in tokens (Claude 4.x).
        /// `0` explicitly disables thinking for this agent. Omit the flag
        /// to leave the current value unchanged. Note: there is no CLI
        /// sentinel to clear back to "inherit server default" today —
        /// matches the HTTP API semantics (see PR #775 S3).
        #[arg(long)]
        thinking_budget_tokens: Option<u32>,
    },
}

pub(crate) fn agent_list(store: &SqliteStore, json: bool) -> anyhow::Result<()> {
    let agents = store.list_agents()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&agents)?);
        return Ok(());
    }
    if agents.is_empty() {
        println!("No agents registered.");
        return Ok(());
    }
    println!(
        "{:<20} {:<10} {:<30} {:<8} LAST ACTIVE",
        "NAME", "ID", "MODEL", "DEFAULT"
    );
    for a in &agents {
        let id_short = short_id(&a.id);
        let model = a.model.as_deref().unwrap_or("(server default)");
        let default = if a.is_default { "*" } else { "" };
        println!(
            "{:<20} {:<10} {:<30} {:<8} {}",
            a.name,
            id_short,
            model,
            default,
            fmt_time(&a.last_active)
        );
    }
    Ok(())
}

/// Options for creating a new agent via the CLI.
pub(crate) struct AgentCreateOpts<'a> {
    pub name: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub posture: Option<String>,
    pub provider: Option<String>,
    /// Anthropic extended-thinking budget override (tokens). `Some(0)`
    /// explicitly disables thinking; `None` inherits the server default.
    /// Matches the HTTP API semantics on `POST /agents`.
    pub thinking_budget_tokens: Option<u32>,
    pub default: bool,
    pub json: bool,
    pub workspace_dir: Option<&'a std::path::Path>,
}

pub(crate) fn agent_create(store: &SqliteStore, opts: AgentCreateOpts<'_>) -> anyhow::Result<()> {
    let AgentCreateOpts {
        name,
        description,
        model,
        posture,
        provider,
        thinking_budget_tokens,
        default,
        json,
        workspace_dir,
    } = opts;

    validate_agent_name(&name)?;

    let now = chrono::Utc::now();
    let agent = AgentRecord {
        id: AgentId::new(),
        name,
        description: description.unwrap_or_default(),
        model,
        posture,
        provider,
        telegram_token: None,
        thinking_budget_tokens,
        is_default: default,
        created_at: now,
        last_active: now,
    };

    if let Err(e) = store.create_agent(&agent) {
        if matches!(&e, alms_core::AlmsError::DuplicateName(_)) {
            anyhow::bail!("Agent name '{}' already exists", agent.name);
        }
        return Err(e.into());
    }

    if default {
        store.set_default_agent(agent.id)?;
    }

    // Create workspace directory and initial files
    if let Some(ws_dir) = workspace_dir {
        let agent_ws_dir = ws_dir.join(&agent.name);
        if let Err(e) = alms_core::init_workspace_files(&agent_ws_dir) {
            eprintln!(
                "Warning: could not create workspace files in {}: {}",
                agent_ws_dir.display(),
                e
            );
        }
    }

    if json {
        let mut val = serde_json::to_value(&agent)?;
        if let Some(ws_dir) = workspace_dir {
            val["workspace_path"] =
                serde_json::Value::String(ws_dir.join(&agent.name).display().to_string());
        }
        println!("{}", serde_json::to_string_pretty(&val)?);
    } else {
        println!("Created agent '{}' ({})", agent.name, agent.id);
        if let Some(ws_dir) = workspace_dir {
            println!("  Workspace: {}", ws_dir.join(&agent.name).display());
        }
    }
    Ok(())
}

pub(crate) fn agent_show(store: &SqliteStore, name_or_id: &str, json: bool) -> anyhow::Result<()> {
    let agent = resolve_agent(store, name_or_id)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&agent)?);
        return Ok(());
    }
    println!("Name:          {}", agent.name);
    println!("ID:            {}", agent.id);
    println!(
        "Description:   {}",
        if agent.description.is_empty() {
            "(none)"
        } else {
            &agent.description
        }
    );
    println!(
        "Model:         {}",
        agent.model.as_deref().unwrap_or("(server default)")
    );
    println!(
        "Posture:       {}",
        agent.posture.as_deref().unwrap_or("(server default)")
    );
    println!(
        "Provider:      {}",
        agent.provider.as_deref().unwrap_or("(server default)")
    );
    // Render the Anthropic extended-thinking budget so operators can verify
    // `--thinking-budget-tokens` landed as expected. `Some(0)` prints as a
    // distinct "disabled (explicit)" so it's visually separable from the
    // inherit-server-default case.
    let thinking_display = match agent.thinking_budget_tokens {
        Some(0) => "disabled (explicit)".to_string(),
        Some(n) => format!("{n} tokens"),
        None => "(server default)".to_string(),
    };
    println!("Thinking:      {thinking_display}");
    println!(
        "Default:       {}",
        if agent.is_default { "yes" } else { "no" }
    );
    println!("Created:       {}", fmt_time(&agent.created_at));
    println!("Last Active:   {}", fmt_time(&agent.last_active));
    Ok(())
}

pub(crate) fn agent_delete(
    store: &SqliteStore,
    name_or_id: &str,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let agent = resolve_agent(store, name_or_id)?;
    if agent.is_default && !force {
        anyhow::bail!(
            "Cannot delete the default agent '{}'. Set another agent as default first, or use --force.",
            agent.name
        );
    }
    store.delete_agent(agent.id)?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "deleted": agent.name })
        );
    } else {
        println!("Deleted agent '{}'", agent.name);
    }
    Ok(())
}

pub(crate) fn agent_set_default(
    store: &SqliteStore,
    name_or_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let agent = resolve_agent(store, name_or_id)?;
    store.set_default_agent(agent.id)?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "default_agent": agent.name })
        );
    } else {
        println!("Set '{}' as default agent", agent.name);
    }
    Ok(())
}

/// Options for updating an agent's configuration via the CLI.
pub(crate) struct AgentConfigOpts<'a> {
    pub name_or_id: &'a str,
    pub model: Option<String>,
    pub posture: Option<String>,
    pub provider: Option<String>,
    pub description: Option<String>,
    /// Anthropic extended-thinking budget override (tokens). `Some(0)`
    /// explicitly disables thinking; `Some(n)` sets the budget. `None`
    /// here means "leave unchanged" — mirrors the HTTP `PATCH /agents`
    /// path where a missing field is a no-op.
    pub thinking_budget_tokens: Option<u32>,
    pub json: bool,
}

pub(crate) fn agent_config(store: &SqliteStore, opts: AgentConfigOpts<'_>) -> anyhow::Result<()> {
    let AgentConfigOpts {
        name_or_id,
        model,
        posture,
        provider,
        description,
        thinking_budget_tokens,
        json,
    } = opts;

    let mut agent = resolve_agent(store, name_or_id)?;

    if let Some(d) = description {
        agent.description = d;
    }
    if let Some(m) = model {
        agent.model = if m.is_empty() { None } else { Some(m) };
    }
    if let Some(p) = posture {
        agent.posture = if p.is_empty() { None } else { Some(p) };
    }
    if let Some(prov) = provider {
        agent.provider = if prov.is_empty() { None } else { Some(prov) };
    }
    // `Some(n)` (including `Some(0)`) is an explicit override matching the
    // HTTP API's `PUT /agents/{id}` semantics. There's no CLI sentinel to
    // clear back to "inherit server default" today — per Tim S3 on #775,
    // we defer that until someone actually hits the trap.
    if let Some(budget) = thinking_budget_tokens {
        agent.thinking_budget_tokens = Some(budget);
    }

    agent.last_active = chrono::Utc::now();
    store.update_agent(&agent)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&agent)?);
    } else {
        println!("Updated agent '{}'", agent.name);
        agent_show(store, &agent.name, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{make_agent, new_store, resolve_agent};

    #[test]
    fn test_create_list_show_delete_roundtrip() {
        let store = new_store();

        // Create
        agent_create(
            &store,
            AgentCreateOpts {
                name: "test-agent".into(),
                description: Some("desc".into()),
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();

        // List
        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "test-agent");
        assert_eq!(agents[0].description, "desc");

        // Show (just verifying resolve works)
        let agent = resolve_agent(&store, "test-agent").unwrap();
        assert_eq!(agent.description, "desc");

        // Delete
        agent_delete(&store, "test-agent", false, false).unwrap();
        assert!(store.list_agents().unwrap().is_empty());
    }

    #[test]
    fn test_create_duplicate_name_fails() {
        let store = new_store();
        agent_create(
            &store,
            AgentCreateOpts {
                name: "dup".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();
        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "dup".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn test_create_invalid_name_fails() {
        let store = new_store();
        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "Bad Name".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("lowercase"));
    }

    #[test]
    fn test_create_creates_workspace_dir() {
        let store = new_store();
        let tmp = tempfile::TempDir::new().unwrap();
        let ws_dir = tmp.path().join("workspace");

        agent_create(
            &store,
            AgentCreateOpts {
                name: "reviewer".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                default: false,
                json: false,
                workspace_dir: Some(&ws_dir),
            },
        )
        .unwrap();

        // Workspace directory and files should exist at {workspace_dir}/{name}/
        let agent_dir = ws_dir.join("reviewer");
        assert!(agent_dir.is_dir());
        for filename in alms_core::WORKSPACE_FILENAMES {
            assert!(
                agent_dir.join(filename).exists(),
                "Expected workspace file {filename} to exist"
            );
        }
    }

    #[test]
    fn test_set_default() {
        let store = new_store();
        make_agent(&store, "alpha");
        make_agent(&store, "beta");

        agent_set_default(&store, "beta", false).unwrap();
        let default = store.get_default_agent().unwrap().unwrap();
        assert_eq!(default.name, "beta");

        // Switch default
        agent_set_default(&store, "alpha", false).unwrap();
        let default = store.get_default_agent().unwrap().unwrap();
        assert_eq!(default.name, "alpha");

        // Verify beta is no longer default
        let beta = resolve_agent(&store, "beta").unwrap();
        assert!(!beta.is_default);
    }

    #[test]
    fn test_delete_default_blocked() {
        let store = new_store();
        let agent = make_agent(&store, "main");
        store.set_default_agent(agent.id).unwrap();

        let err = agent_delete(&store, "main", false, false).unwrap_err();
        assert!(err.to_string().contains("Cannot delete the default"));
    }

    #[test]
    fn test_delete_default_with_force() {
        let store = new_store();
        let agent = make_agent(&store, "main");
        store.set_default_agent(agent.id).unwrap();

        agent_delete(&store, "main", true, false).unwrap();
        assert!(store.list_agents().unwrap().is_empty());
    }

    #[test]
    fn test_config_update() {
        let store = new_store();
        make_agent(&store, "configurable");

        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "configurable",
                model: Some("new-model".into()),
                posture: Some("guarded".into()),
                provider: None,
                description: Some("updated desc".into()),
                thinking_budget_tokens: None,
                json: false,
            },
        )
        .unwrap();

        let agent = resolve_agent(&store, "configurable").unwrap();
        assert_eq!(agent.model.as_deref(), Some("new-model"));
        assert_eq!(agent.posture.as_deref(), Some("guarded"));
        assert_eq!(agent.description, "updated desc");
    }

    #[test]
    fn test_config_clear_override() {
        let store = new_store();
        let now = chrono::Utc::now();
        let agent = AgentRecord {
            id: AgentId::new(),
            name: "clearable".to_string(),
            description: String::new(),
            model: Some("old-model".to_string()),
            posture: Some("guarded".to_string()),
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            is_default: false,
            created_at: now,
            last_active: now,
        };
        store.create_agent(&agent).unwrap();

        // Clear model by passing empty string
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "clearable",
                model: Some(String::new()),
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                json: false,
            },
        )
        .unwrap();
        let updated = resolve_agent(&store, "clearable").unwrap();
        assert!(updated.model.is_none());
        // Posture untouched
        assert_eq!(updated.posture.as_deref(), Some("guarded"));
    }

    // -- CLI --thinking-budget-tokens flag on create + update (PR #775 S5) ------
    //
    // `alms agent create --thinking-budget-tokens N` must persist the value
    // verbatim (including `Some(0)` as an explicit per-agent disable).
    // `alms agent config --thinking-budget-tokens N` must overwrite an
    // existing value, mirroring the HTTP API's `PUT /agents/{id}` behaviour.
    #[test]
    fn test_create_with_thinking_budget() {
        let store = new_store();
        agent_create(
            &store,
            AgentCreateOpts {
                name: "thinker".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: Some(8192),
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();
        let agent = resolve_agent(&store, "thinker").unwrap();
        assert_eq!(agent.thinking_budget_tokens, Some(8192));
    }

    #[test]
    fn test_create_with_thinking_budget_zero_disables() {
        // `--thinking-budget-tokens 0` is an explicit per-agent opt-out, not
        // "inherit server default". Must survive round-trip as `Some(0)`.
        let store = new_store();
        agent_create(
            &store,
            AgentCreateOpts {
                name: "no-thinker".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: Some(0),
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();
        let agent = resolve_agent(&store, "no-thinker").unwrap();
        assert_eq!(agent.thinking_budget_tokens, Some(0));
    }

    #[test]
    fn test_config_updates_thinking_budget() {
        let store = new_store();
        make_agent(&store, "tuner");

        // Set an initial value.
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "tuner",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: Some(4096),
                json: false,
            },
        )
        .unwrap();
        let after_set = resolve_agent(&store, "tuner").unwrap();
        assert_eq!(after_set.thinking_budget_tokens, Some(4096));

        // Overwrite with an explicit disable (`Some(0)`).
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "tuner",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: Some(0),
                json: false,
            },
        )
        .unwrap();
        let after_zero = resolve_agent(&store, "tuner").unwrap();
        assert_eq!(after_zero.thinking_budget_tokens, Some(0));

        // Omitting the flag (`None`) must leave the stored value untouched.
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "tuner",
                model: Some("some-model".into()),
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                json: false,
            },
        )
        .unwrap();
        let after_noop = resolve_agent(&store, "tuner").unwrap();
        assert_eq!(
            after_noop.thinking_budget_tokens,
            Some(0),
            "omitted flag must not clobber existing thinking_budget_tokens"
        );
        assert_eq!(after_noop.model.as_deref(), Some("some-model"));
    }
}
