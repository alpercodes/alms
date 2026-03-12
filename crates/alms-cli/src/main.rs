use alms_core::{AgentId, AgentRecord, SessionId, validate_agent_name};
use alms_session::{Session, SessionStatus, SqliteStore};
use clap::{Parser, Subcommand};
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "alms")]
#[command(about = "ALMS - Agent Loop Management System")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the gateway server
    Gateway {
        /// Bind address
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        bind: String,
        /// OpenRouter/OpenAI API key (overrides OPENROUTER_API_KEY env var)
        #[arg(long, env = "OPENROUTER_API_KEY")]
        api_key: Option<String>,
    },
    /// Check system health
    Health {
        /// Gateway URL to check
        #[arg(short, long, default_value = "http://127.0.0.1:8080")]
        url: String,
    },
    /// Manage sessions
    Session {
        #[command(subcommand)]
        cmd: SessionCommands,
        /// Output as JSON instead of human-readable text
        #[arg(long, global = true)]
        json: bool,
    },
    /// Manage agents
    Agent {
        #[command(subcommand)]
        cmd: AgentCommands,
        /// Output as JSON instead of human-readable text
        #[arg(long, global = true)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AgentCommands {
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
        /// Posture override ("guarded" or "full_control")
        #[arg(long)]
        posture: Option<String>,
        /// System prompt override
        #[arg(long)]
        system_prompt: Option<String>,
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
        /// System prompt override (empty string to clear)
        #[arg(long)]
        system_prompt: Option<String>,
        /// Description
        #[arg(long)]
        description: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum SessionCommands {
    /// List sessions (optionally filtered by agent)
    List {
        /// Filter by agent name or UUID
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show details of a specific session
    Show {
        /// Session UUID
        session_id: String,
    },
    /// Delete a session and all its messages
    Delete {
        /// Session UUID
        session_id: String,
    },
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Open the SQLite store at the configured DB path.
fn open_db() -> anyhow::Result<SqliteStore> {
    let db_path = std::env::var("ALMS_DB_PATH").unwrap_or_else(|_| "./data/alms.db".to_string());
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(SqliteStore::open(&db_path)?)
}

/// Resolve an agent by UUID or name slug.
fn resolve_agent(store: &SqliteStore, name_or_id: &str) -> anyhow::Result<AgentRecord> {
    if let Ok(uuid) = uuid::Uuid::parse_str(name_or_id) {
        if let Some(agent) = store.load_agent_by_id(AgentId(uuid))? {
            return Ok(agent);
        }
        anyhow::bail!("Agent not found: {name_or_id}");
    }
    match store.load_agent_by_name(name_or_id)? {
        Some(agent) => Ok(agent),
        None => anyhow::bail!("Agent not found: {name_or_id}"),
    }
}

/// Format a chrono DateTime for display.
fn fmt_time(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M UTC").to_string()
}

// ---------------------------------------------------------------------------
// Agent command handlers
// ---------------------------------------------------------------------------

fn agent_list(store: &SqliteStore, json: bool) -> anyhow::Result<()> {
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
        let id_short = &a.id.to_string()[..8];
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

#[allow(clippy::too_many_arguments)]
fn agent_create(
    store: &SqliteStore,
    name: String,
    description: Option<String>,
    model: Option<String>,
    posture: Option<String>,
    system_prompt: Option<String>,
    default: bool,
    json: bool,
) -> anyhow::Result<()> {
    validate_agent_name(&name)?;

    let now = chrono::Utc::now();
    let agent = AgentRecord {
        id: AgentId::new(),
        name,
        description: description.unwrap_or_default(),
        model,
        system_prompt,
        posture,
        is_default: default,
        created_at: now,
        last_active: now,
    };

    if let Err(e) = store.create_agent(&agent) {
        let msg = e.to_string();
        if msg.contains("UNIQUE") {
            anyhow::bail!("Agent name '{}' already exists", agent.name);
        }
        return Err(e.into());
    }

    if default {
        store.set_default_agent(agent.id)?;
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&agent)?);
    } else {
        println!("Created agent '{}' ({})", agent.name, agent.id);
    }
    Ok(())
}

fn agent_show(store: &SqliteStore, name_or_id: &str, json: bool) -> anyhow::Result<()> {
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
        "System Prompt: {}",
        if agent.system_prompt.is_some() {
            "(custom)"
        } else {
            "(server default)"
        }
    );
    println!(
        "Default:       {}",
        if agent.is_default { "yes" } else { "no" }
    );
    println!("Created:       {}", fmt_time(&agent.created_at));
    println!("Last Active:   {}", fmt_time(&agent.last_active));
    Ok(())
}

fn agent_delete(
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

fn agent_set_default(store: &SqliteStore, name_or_id: &str, json: bool) -> anyhow::Result<()> {
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

fn agent_config(
    store: &SqliteStore,
    name_or_id: &str,
    model: Option<String>,
    posture: Option<String>,
    system_prompt: Option<String>,
    description: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
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
    if let Some(sp) = system_prompt {
        agent.system_prompt = if sp.is_empty() { None } else { Some(sp) };
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

// ---------------------------------------------------------------------------
// Session command handlers
// ---------------------------------------------------------------------------

fn session_list(store: &SqliteStore, agent: Option<String>, json: bool) -> anyhow::Result<()> {
    let sessions: Vec<Session> = if let Some(ref name_or_id) = agent {
        let agent = resolve_agent(store, name_or_id)?;
        store.load_sessions_by_agent(agent.id)?
    } else {
        store.list_sessions()?
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
        return Ok(());
    }
    if sessions.is_empty() {
        if let Some(ref a) = agent {
            println!("No sessions found for agent '{a}'.");
        } else {
            println!("No sessions found.");
        }
        return Ok(());
    }

    println!(
        "{:<12} {:<12} {:<10} {:<8} {:<22} LAST ACTIVITY",
        "SESSION", "AGENT", "STATUS", "MSGS", "CREATED"
    );
    for s in &sessions {
        let id_str = s.id.0.to_string();
        let id_short = &id_str[..8];
        let agent_short = &s.agent_id.to_string()[..8];
        let status = match s.status {
            SessionStatus::Active => "active",
            SessionStatus::Idle => "idle",
            SessionStatus::Archived => "archived",
        };
        let msg_count = store.message_count(s.id).unwrap_or(0);
        println!(
            "{:<12} {:<12} {:<10} {:<8} {:<22} {}",
            id_short,
            agent_short,
            status,
            msg_count,
            fmt_time(&s.created_at.0),
            fmt_time(&s.last_activity.0),
        );
    }
    Ok(())
}

fn session_show(store: &SqliteStore, session_id_str: &str, json: bool) -> anyhow::Result<()> {
    let uuid =
        uuid::Uuid::parse_str(session_id_str).map_err(|_| anyhow::anyhow!("Invalid UUID"))?;
    let sid = SessionId(uuid);
    let session = store
        .load_session_by_id(sid)?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id_str}"))?;

    let msg_count = store.message_count(sid).unwrap_or(0);

    if json {
        let mut val = serde_json::to_value(&session)?;
        val.as_object_mut()
            .unwrap()
            .insert("message_count".into(), serde_json::json!(msg_count));
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }

    let status = match session.status {
        SessionStatus::Active => "active",
        SessionStatus::Idle => "idle",
        SessionStatus::Archived => "archived",
    };

    println!("Session:       {}", session.id.0);
    println!("Agent:         {}", session.agent_id);
    println!("Context:       {}", session.context_id);
    println!("Status:        {}", status);
    println!("Messages:      {}", msg_count);
    println!("Created:       {}", fmt_time(&session.created_at.0));
    println!("Last Activity: {}", fmt_time(&session.last_activity.0));

    // Try to resolve agent name for a friendlier display
    if let Ok(Some(agent)) = store.load_agent_by_id(session.agent_id) {
        println!("Agent Name:    {}", agent.name);
    }
    Ok(())
}

fn session_delete(store: &SqliteStore, session_id_str: &str, json: bool) -> anyhow::Result<()> {
    let uuid =
        uuid::Uuid::parse_str(session_id_str).map_err(|_| anyhow::anyhow!("Invalid UUID"))?;
    let sid = SessionId(uuid);

    // Verify session exists before deleting
    store
        .load_session_by_id(sid)?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id_str}"))?;

    store.delete_session(sid)?;

    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "deleted": session_id_str })
        );
    } else {
        println!("Deleted session {session_id_str}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Gateway { bind, api_key } => {
            info!("Starting ALMS Gateway...");
            match &api_key {
                Some(k) => info!("API key provided ({} chars)", k.len()),
                None => {
                    let found: Vec<String> = std::env::vars()
                        .filter(|(k, _)| k.contains("API_KEY") || k.contains("OPENROUTER"))
                        .map(|(k, v)| format!("{}=({} chars)", k, v.len()))
                        .collect();
                    if found.is_empty() {
                        error!(
                            "No API key found. Pass --api-key sk-or-... or set OPENROUTER_API_KEY."
                        );
                    } else {
                        warn!("API key env vars visible to process: {}", found.join(", "));
                    }
                }
            }
            if let Some(key) = api_key {
                unsafe {
                    std::env::set_var("OPENROUTER_API_KEY", key);
                }
            }
            alms_gateway::serve(&bind).await?;
        }
        Commands::Health { url } => {
            let health_url = format!("{}/health", url.trim_end_matches('/'));
            match reqwest::get(&health_url).await {
                Ok(resp) if resp.status().is_success() => {
                    let body: serde_json::Value = resp.json().await?;
                    println!("ALMS Gateway is healthy");
                    if let Some(version) = body.get("version").and_then(|v| v.as_str()) {
                        println!("  version: {}", version);
                    }
                }
                Ok(resp) => {
                    eprintln!("Health check failed: HTTP {}", resp.status());
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("Cannot reach gateway at {}: {}", health_url, e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Session { cmd, json } => {
            let store = open_db()?;
            match cmd {
                SessionCommands::List { agent } => session_list(&store, agent, json)?,
                SessionCommands::Show { session_id } => {
                    session_show(&store, &session_id, json)?;
                }
                SessionCommands::Delete { session_id } => {
                    session_delete(&store, &session_id, json)?;
                }
            }
        }
        Commands::Agent { cmd, json } => {
            let store = open_db()?;
            match cmd {
                AgentCommands::List => agent_list(&store, json)?,
                AgentCommands::Create {
                    name,
                    description,
                    model,
                    posture,
                    system_prompt,
                    default,
                } => {
                    agent_create(
                        &store,
                        name,
                        description,
                        model,
                        posture,
                        system_prompt,
                        default,
                        json,
                    )?;
                }
                AgentCommands::Show { name_or_id } => agent_show(&store, &name_or_id, json)?,
                AgentCommands::Delete { name_or_id, force } => {
                    agent_delete(&store, &name_or_id, force, json)?;
                }
                AgentCommands::SetDefault { name_or_id } => {
                    agent_set_default(&store, &name_or_id, json)?;
                }
                AgentCommands::Config {
                    name_or_id,
                    model,
                    posture,
                    system_prompt,
                    description,
                } => {
                    agent_config(
                        &store,
                        &name_or_id,
                        model,
                        posture,
                        system_prompt,
                        description,
                        json,
                    )?;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_store() -> SqliteStore {
        SqliteStore::open_in_memory().unwrap()
    }

    fn make_agent(store: &SqliteStore, name: &str) -> AgentRecord {
        let now = chrono::Utc::now();
        let agent = AgentRecord {
            id: AgentId::new(),
            name: name.to_string(),
            description: String::new(),
            model: None,
            system_prompt: None,
            posture: None,
            is_default: false,
            created_at: now,
            last_active: now,
        };
        store.create_agent(&agent).unwrap();
        agent
    }

    #[test]
    fn test_resolve_agent_by_name() {
        let store = new_store();
        let agent = make_agent(&store, "atlas");
        let resolved = resolve_agent(&store, "atlas").unwrap();
        assert_eq!(resolved.id, agent.id);
    }

    #[test]
    fn test_resolve_agent_by_uuid() {
        let store = new_store();
        let agent = make_agent(&store, "atlas");
        let resolved = resolve_agent(&store, &agent.id.to_string()).unwrap();
        assert_eq!(resolved.name, "atlas");
    }

    #[test]
    fn test_resolve_agent_not_found() {
        let store = new_store();
        assert!(resolve_agent(&store, "nonexistent").is_err());
    }

    #[test]
    fn test_create_list_show_delete_roundtrip() {
        let store = new_store();

        // Create
        agent_create(
            &store,
            "test-agent".into(),
            Some("desc".into()),
            None,
            None,
            None,
            false,
            false,
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
        agent_create(&store, "dup".into(), None, None, None, None, false, false).unwrap();
        let err =
            agent_create(&store, "dup".into(), None, None, None, None, false, false).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn test_create_invalid_name_fails() {
        let store = new_store();
        let err = agent_create(
            &store,
            "Bad Name".into(),
            None,
            None,
            None,
            None,
            false,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("lowercase"));
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
            "configurable",
            Some("new-model".into()),
            Some("guarded".into()),
            None,
            Some("updated desc".into()),
            false,
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
            system_prompt: None,
            posture: Some("guarded".to_string()),
            is_default: false,
            created_at: now,
            last_active: now,
        };
        store.create_agent(&agent).unwrap();

        // Clear model by passing empty string
        agent_config(
            &store,
            "clearable",
            Some(String::new()),
            None,
            None,
            None,
            false,
        )
        .unwrap();
        let updated = resolve_agent(&store, "clearable").unwrap();
        assert!(updated.model.is_none());
        // Posture untouched
        assert_eq!(updated.posture.as_deref(), Some("guarded"));
    }

    // ── Session tests ────────────────────────────────────────────────────

    fn make_session(store: &SqliteStore, agent_id: AgentId) -> Session {
        let session = Session::new(agent_id, "default");
        store.save_session(&session).unwrap();
        session
    }

    #[test]
    fn test_session_list_empty() {
        let store = new_store();
        session_list(&store, None, false).unwrap();
    }

    #[test]
    fn test_session_list_all() {
        let store = new_store();
        let agent = make_agent(&store, "sess-agent");
        make_session(&store, agent.id);
        make_session(&store, agent.id);

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn test_session_list_by_agent() {
        let store = new_store();
        let a1 = make_agent(&store, "agent-a");
        let a2 = make_agent(&store, "agent-b");
        make_session(&store, a1.id);
        make_session(&store, a1.id);
        make_session(&store, a2.id);

        let s1 = store.load_sessions_by_agent(a1.id).unwrap();
        assert_eq!(s1.len(), 2);
        let s2 = store.load_sessions_by_agent(a2.id).unwrap();
        assert_eq!(s2.len(), 1);
    }

    #[test]
    fn test_session_show() {
        let store = new_store();
        let agent = make_agent(&store, "show-agent");
        let session = make_session(&store, agent.id);

        session_show(&store, &session.id.0.to_string(), false).unwrap();
    }

    #[test]
    fn test_session_show_not_found() {
        let store = new_store();
        let err = session_show(&store, &uuid::Uuid::new_v4().to_string(), false).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_session_show_invalid_uuid() {
        let store = new_store();
        let err = session_show(&store, "not-a-uuid", false).unwrap_err();
        assert!(err.to_string().contains("Invalid UUID"));
    }

    #[test]
    fn test_session_delete() {
        let store = new_store();
        let agent = make_agent(&store, "del-agent");
        let session = make_session(&store, agent.id);

        session_delete(&store, &session.id.0.to_string(), false).unwrap();
        assert!(store.load_session_by_id(session.id).unwrap().is_none());
    }

    #[test]
    fn test_session_delete_not_found() {
        let store = new_store();
        let err = session_delete(&store, &uuid::Uuid::new_v4().to_string(), false).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
