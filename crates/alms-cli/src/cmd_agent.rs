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
    pub json: bool,
}

pub(crate) fn agent_config(store: &SqliteStore, opts: AgentConfigOpts<'_>) -> anyhow::Result<()> {
    let AgentConfigOpts {
        name_or_id,
        model,
        posture,
        provider,
        description,
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
                json: false,
            },
        )
        .unwrap();
        let updated = resolve_agent(&store, "clearable").unwrap();
        assert!(updated.model.is_none());
        // Posture untouched
        assert_eq!(updated.posture.as_deref(), Some("guarded"));
    }
}
