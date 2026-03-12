use alms_core::job::{Job, JobId, JobSchedule, JobStatus};
use alms_core::{
    AgentId, AgentRecord, CreateJobRequest, CreateRunRequest, CreateRunResponse, RunInput,
    RunStatusResponse, SessionId, validate_agent_name,
};
use alms_session::{Session, SqliteStore};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
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
        #[arg(
            short,
            long,
            default_value = "http://127.0.0.1:8080",
            env = "ALMS_GATEWAY_URL"
        )]
        url: String,
        /// Output as JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },
    /// Open the ALMS web UI in your browser
    Dashboard {
        /// Gateway URL to open
        #[arg(
            short,
            long,
            default_value = "http://127.0.0.1:8080",
            env = "ALMS_GATEWAY_URL"
        )]
        url: String,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
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
    /// Manage runs (requires running gateway)
    Run {
        #[command(subcommand)]
        cmd: RunCommands,
        /// Output as JSON instead of human-readable text
        #[arg(long, global = true)]
        json: bool,
        /// Gateway URL
        #[arg(
            long,
            global = true,
            default_value = "http://127.0.0.1:8080",
            env = "ALMS_GATEWAY_URL"
        )]
        url: String,
    },
    /// Manage scheduled jobs
    Job {
        #[command(subcommand)]
        cmd: JobCommands,
        /// Output as JSON instead of human-readable text
        #[arg(long, global = true)]
        json: bool,
        /// Gateway URL (used by create/cancel)
        #[arg(
            long,
            global = true,
            default_value = "http://127.0.0.1:8080",
            env = "ALMS_GATEWAY_URL"
        )]
        url: String,
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

#[derive(Subcommand, Debug)]
enum RunCommands {
    /// Create a new run (requires running gateway)
    Create {
        /// Session UUID
        #[arg(long)]
        session: String,
        /// Input text for the agent
        #[arg(long)]
        input: String,
        /// Model override
        #[arg(long)]
        model: Option<String>,
        /// Temperature override (0.0-2.0)
        #[arg(long)]
        temperature: Option<f32>,
        /// Max tokens override
        #[arg(long)]
        max_tokens: Option<u32>,
        /// Posture override ("guarded" or "full_control")
        #[arg(long)]
        posture: Option<String>,
    },
    /// List runs for a session (requires running gateway)
    List {
        /// Session UUID
        #[arg(long)]
        session: String,
        /// Max results
        #[arg(long, default_value = "50")]
        limit: usize,
    },
    /// Show details of a run (requires running gateway)
    Show {
        /// Run UUID
        run_id: String,
    },
}

#[derive(Subcommand, Debug)]
enum JobCommands {
    /// List all jobs
    List {
        /// Filter by agent name or UUID
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show details of a job
    Show {
        /// Job UUID
        job_id: String,
    },
    /// Create a new job (requires running gateway)
    Create {
        /// Agent name or UUID
        #[arg(long)]
        agent: String,
        /// Prompt text for the agent
        #[arg(long)]
        prompt: String,
        /// Schedule: "once:2026-03-15T09:00:00Z" or "cron:0 9 * * 1-5"
        #[arg(long)]
        schedule: String,
    },
    /// Cancel a job (requires running gateway)
    Cancel {
        /// Job UUID
        job_id: String,
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

/// Truncate a UUID string to the first 8 characters for display.
fn short_id(id: &impl std::fmt::Display) -> String {
    let s = id.to_string();
    s.get(..8).unwrap_or(&s).to_string()
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
        if matches!(&e, alms_core::AlmsError::DuplicateName(_)) {
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
        let id_short = short_id(&s.id.0);
        let agent_short = short_id(&s.agent_id);
        let msg_count = store.message_count(s.id).unwrap_or(0);
        println!(
            "{:<12} {:<12} {:<10} {:<8} {:<22} {}",
            id_short,
            agent_short,
            s.status,
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

    println!("Session:       {}", session.id.0);
    println!("Agent:         {}", session.agent_id);
    if let Ok(Some(agent)) = store.load_agent_by_id(session.agent_id) {
        println!("Agent Name:    {}", agent.name);
    }
    println!("Context:       {}", session.context_id);
    println!("Status:        {}", session.status);
    println!("Messages:      {}", msg_count);
    println!("Created:       {}", fmt_time(&session.created_at.0));
    println!("Last Activity: {}", fmt_time(&session.last_activity.0));
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
// HTTP helpers
// ---------------------------------------------------------------------------

/// Build a full URL from base and path.
fn api_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Build a reqwest client with optional auth token.
fn api_client() -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();
    if let Ok(token) = std::env::var("ALMS_AUTH_TOKEN") {
        let mut headers = reqwest::header::HeaderMap::new();
        let val = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| anyhow::anyhow!("ALMS_AUTH_TOKEN contains invalid characters for an HTTP header"))?;
        headers.insert(reqwest::header::AUTHORIZATION, val);
        builder = builder.default_headers(headers);
    }
    Ok(builder.build().unwrap_or_else(|_| reqwest::Client::new()))
}

/// Parse an HTTP error response body into a user-friendly message.
fn parse_api_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(msg) = val
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
    {
        return format!("HTTP {status}: {msg}");
    }
    format!("HTTP {status}: {body}")
}

/// Send a GET request and return the response body as JSON Value.
async fn api_get(base_url: &str, path: &str) -> anyhow::Result<serde_json::Value> {
    let client = api_client()?;
    let url = api_url(base_url, path);
    let resp = client.get(&url).send().await.map_err(|e| {
        if e.is_connect() {
            anyhow::anyhow!(
                "Cannot connect to gateway at {base_url}. Is it running? Start with: alms gateway"
            )
        } else {
            anyhow::anyhow!("Request failed: {e}")
        }
    })?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("{}", parse_api_error(status, &body));
    }
    Ok(serde_json::from_str(&body)?)
}

/// Send a POST request with JSON body and return the response.
async fn api_post(
    base_url: &str,
    path: &str,
    body: &impl serde::Serialize,
) -> anyhow::Result<(reqwest::StatusCode, serde_json::Value)> {
    let client = api_client()?;
    let url = api_url(base_url, path);
    let resp = client.post(&url).json(body).send().await.map_err(|e| {
        if e.is_connect() {
            anyhow::anyhow!(
                "Cannot connect to gateway at {base_url}. Is it running? Start with: alms gateway"
            )
        } else {
            anyhow::anyhow!("Request failed: {e}")
        }
    })?;
    let status = resp.status();
    let body_text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("{}", parse_api_error(status, &body_text));
    }
    Ok((status, serde_json::from_str(&body_text)?))
}

/// Send a DELETE request and return the status code.
async fn api_delete(base_url: &str, path: &str) -> anyhow::Result<reqwest::StatusCode> {
    let client = api_client()?;
    let url = api_url(base_url, path);
    let resp = client.delete(&url).send().await.map_err(|e| {
        if e.is_connect() {
            anyhow::anyhow!(
                "Cannot connect to gateway at {base_url}. Is it running? Start with: alms gateway"
            )
        } else {
            anyhow::anyhow!("Request failed: {e}")
        }
    })?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await?;
        anyhow::bail!("{}", parse_api_error(status, &body));
    }
    Ok(status)
}

// ---------------------------------------------------------------------------
// Run command handlers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_create(
    url: &str,
    session: &str,
    input: &str,
    model: Option<String>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    posture: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let session_uuid =
        uuid::Uuid::parse_str(session).map_err(|_| anyhow::anyhow!("Invalid session UUID"))?;
    let req = CreateRunRequest {
        session_id: SessionId(session_uuid),
        input: RunInput::Text {
            text: input.to_string(),
        },
        model,
        temperature,
        max_tokens,
        posture,
    };
    let (_status, val) = api_post(url, "runs", &req).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&val)?);
    } else {
        let resp: CreateRunResponse = serde_json::from_value(val)?;
        println!("Created run {} (status: {:?})", resp.run_id.0, resp.status);
    }
    Ok(())
}

async fn run_list(url: &str, session: &str, limit: usize, json: bool) -> anyhow::Result<()> {
    let _uuid =
        uuid::Uuid::parse_str(session).map_err(|_| anyhow::anyhow!("Invalid session UUID"))?;
    let path = format!("runs?session_id={session}&limit={limit}");
    let val = api_get(url, &path).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }

    let runs: Vec<RunStatusResponse> = serde_json::from_value(
        val.get("runs")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![])),
    )?;

    if runs.is_empty() {
        println!("No runs found for session {session}.");
        return Ok(());
    }

    println!(
        "{:<12} {:<12} {:<12} {:<12} STARTED",
        "RUN", "SESSION", "AGENT", "STATUS"
    );
    for r in &runs {
        let run_short = short_id(&r.run_id.0);
        let sess_short = short_id(&r.session_id.0);
        let agent_short = short_id(&r.agent_id);
        let status = format!("{:?}", r.status).to_lowercase();
        let started = r
            .started_at
            .map(|t| fmt_time(&t))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<12} {:<12} {:<12} {:<12} {}",
            run_short, sess_short, agent_short, status, started
        );
    }
    Ok(())
}

async fn run_show(url: &str, run_id: &str, json: bool) -> anyhow::Result<()> {
    let _uuid = uuid::Uuid::parse_str(run_id).map_err(|_| anyhow::anyhow!("Invalid run UUID"))?;
    let val = api_get(url, &format!("runs/{run_id}")).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }

    let r: RunStatusResponse = serde_json::from_value(val)?;
    let status = format!("{:?}", r.status).to_lowercase();
    println!("Run:         {}", r.run_id.0);
    println!("Session:     {}", r.session_id.0);
    println!("Agent:       {}", r.agent_id);
    println!("Status:      {}", status);
    if let Some(t) = r.started_at {
        println!("Started:     {}", fmt_time(&t));
    }
    if let Some(t) = r.ended_at {
        println!("Ended:       {}", fmt_time(&t));
    }
    if let Some(u) = r.usage {
        println!(
            "Tokens:      prompt={}, completion={}",
            u.prompt_tokens, u.completion_tokens
        );
    }
    if let Some(jid) = r.job_id {
        println!("Job:         {}", jid);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Job command handlers
// ---------------------------------------------------------------------------

fn job_list(store: &SqliteStore, agent: Option<String>, json: bool) -> anyhow::Result<()> {
    let mut jobs = store.load_all_jobs_unfiltered()?;

    if let Some(ref name_or_id) = agent {
        let agent = resolve_agent(store, name_or_id)?;
        jobs.retain(|j| j.agent_id == agent.id);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&jobs)?);
        return Ok(());
    }

    if jobs.is_empty() {
        if let Some(ref a) = agent {
            println!("No jobs found for agent '{a}'.");
        } else {
            println!("No jobs found.");
        }
        return Ok(());
    }

    println!(
        "{:<12} {:<12} {:<22} {:<12} {:<22} LAST RUN",
        "JOB", "AGENT", "SCHEDULE", "STATUS", "NEXT RUN"
    );
    for j in &jobs {
        let id_short = short_id(&j.id);
        let agent_short = short_id(&j.agent_id);
        let schedule = match &j.schedule {
            JobSchedule::Once { run_at } => format!("once:{}", fmt_time(run_at)),
            JobSchedule::Recurring { cron } => format!("cron:{cron}"),
        };
        let status = fmt_job_status(j.status);
        let next = j
            .next_run_at
            .map(|t| fmt_time(&t))
            .unwrap_or_else(|| "-".to_string());
        let last = j
            .last_run_at
            .map(|t| fmt_time(&t))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<12} {:<12} {:<22} {:<12} {:<22} {}",
            id_short, agent_short, schedule, status, next, last
        );
    }
    Ok(())
}

fn job_show(store: &SqliteStore, job_id_str: &str, json: bool) -> anyhow::Result<()> {
    let uuid =
        uuid::Uuid::parse_str(job_id_str).map_err(|_| anyhow::anyhow!("Invalid job UUID"))?;
    let job = store
        .load_job_by_id(JobId(uuid))?
        .ok_or_else(|| anyhow::anyhow!("Job not found: {job_id_str}"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&job)?);
        return Ok(());
    }

    let schedule = match &job.schedule {
        JobSchedule::Once { run_at } => format!("once ({})", fmt_time(run_at)),
        JobSchedule::Recurring { cron } => format!("recurring ({cron})"),
    };

    println!("Job:         {}", job.id);
    println!("Agent:       {}", job.agent_id);
    println!("Prompt:      {}", job.prompt);
    println!("Schedule:    {}", schedule);
    println!("Status:      {}", fmt_job_status(job.status));
    println!("Created:     {}", fmt_time(&job.created_at));
    if let Some(t) = job.next_run_at {
        println!("Next Run:    {}", fmt_time(&t));
    }
    if let Some(t) = job.last_run_at {
        println!("Last Run:    {}", fmt_time(&t));
    }

    // Try to resolve agent name
    if let Ok(Some(agent)) = store.load_agent_by_id(job.agent_id) {
        println!("Agent Name:  {}", agent.name);
    }
    Ok(())
}

async fn job_create(
    url: &str,
    store: &SqliteStore,
    agent_name_or_id: &str,
    prompt: &str,
    schedule_str: &str,
    json: bool,
) -> anyhow::Result<()> {
    let agent = resolve_agent(store, agent_name_or_id)?;
    let schedule = parse_schedule(schedule_str)?;

    let req = CreateJobRequest {
        agent_id: agent.id,
        prompt: prompt.to_string(),
        schedule,
    };
    let (_status, val) = api_post(url, "jobs", &req).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&val)?);
    } else {
        let created: Job = serde_json::from_value(val)?;
        println!("Created job {} for agent '{}'", created.id, agent.name);
    }
    Ok(())
}

async fn job_cancel(url: &str, job_id_str: &str, json: bool) -> anyhow::Result<()> {
    uuid::Uuid::parse_str(job_id_str).map_err(|_| anyhow::anyhow!("Invalid job UUID"))?;
    api_delete(url, &format!("jobs/{job_id_str}")).await?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "cancelled": job_id_str })
        );
    } else {
        println!("Cancelled job {job_id_str}");
    }
    Ok(())
}

/// Parse a schedule string: "once:2026-03-15T09:00:00Z" or "cron:0 9 * * 1-5"
fn parse_schedule(s: &str) -> anyhow::Result<JobSchedule> {
    if let Some(rest) = s.strip_prefix("once:") {
        let run_at = chrono::DateTime::parse_from_rfc3339(rest)
            .map_err(|e| anyhow::anyhow!("Invalid once timestamp (must be RFC 3339): {e}"))?
            .with_timezone(&chrono::Utc);
        return Ok(JobSchedule::Once { run_at });
    }
    if let Some(rest) = s.strip_prefix("cron:") {
        let cron_str = rest.trim().to_string();
        // Validate by parsing with the cron crate (6-field: sec prepended).
        // This matches what the gateway scheduler does in cron_utils.rs.
        let six_field = format!("0 {cron_str}");
        six_field.parse::<cron::Schedule>().map_err(|e| {
            anyhow::anyhow!("Invalid cron expression '{cron_str}': {e}")
        })?;
        return Ok(JobSchedule::Recurring { cron: cron_str });
    }
    anyhow::bail!("Invalid schedule format. Use 'once:2026-03-15T09:00:00Z' or 'cron:0 9 * * 1-5'");
}

fn fmt_job_status(s: JobStatus) -> &'static str {
    match s {
        JobStatus::Pending => "pending",
        JobStatus::Active => "active",
        JobStatus::Cancelled => "cancelled",
    }
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
        Commands::Health { url, json } => {
            let health_url = format!("{}/health", url.trim_end_matches('/'));
            match reqwest::get(&health_url).await {
                Ok(resp) if resp.status().is_success() => {
                    let body: serde_json::Value = resp.json().await?;
                    if json {
                        // Wrap with "ok" key for consistent envelope across success/error
                        let mut envelope = body;
                        if let Some(obj) = envelope.as_object_mut() {
                            obj.insert("ok".into(), serde_json::json!(true));
                        }
                        println!("{}", serde_json::to_string_pretty(&envelope)?);
                    } else {
                        println!("ALMS Gateway is healthy");
                        if let Some(version) = body.get("version").and_then(|v| v.as_str()) {
                            println!("  version: {}", version);
                        }
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({ "ok": false, "error": format!("HTTP {status}") })
                        );
                    } else {
                        eprintln!("Health check failed: HTTP {}", status);
                    }
                    std::process::exit(1);
                }
                Err(e) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({ "ok": false, "error": format!("Cannot reach gateway: {e}") })
                        );
                    } else {
                        eprintln!("Cannot reach gateway at {}: {}", health_url, e);
                    }
                    std::process::exit(1);
                }
            }
        }
        Commands::Dashboard { url } => {
            let url = url.trim_end_matches('/').to_string();
            println!("Opening {url} ...");
            if let Err(e) = open::that(&url) {
                eprintln!("Failed to open browser: {e}");
                eprintln!("Open manually: {url}");
                std::process::exit(1);
            }
        }
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "alms", &mut std::io::stdout());
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
        Commands::Run { cmd, json, url } => match cmd {
            RunCommands::Create {
                session,
                input,
                model,
                temperature,
                max_tokens,
                posture,
            } => {
                run_create(
                    &url,
                    &session,
                    &input,
                    model,
                    temperature,
                    max_tokens,
                    posture,
                    json,
                )
                .await?;
            }
            RunCommands::List { session, limit } => {
                run_list(&url, &session, limit, json).await?;
            }
            RunCommands::Show { run_id } => {
                run_show(&url, &run_id, json).await?;
            }
        },
        Commands::Job { cmd, json, url } => match cmd {
            JobCommands::List { agent } => {
                let store = open_db()?;
                job_list(&store, agent, json)?;
            }
            JobCommands::Show { job_id } => {
                let store = open_db()?;
                job_show(&store, &job_id, json)?;
            }
            JobCommands::Create {
                agent,
                prompt,
                schedule,
            } => {
                let store = open_db()?;
                job_create(&url, &store, &agent, &prompt, &schedule, json).await?;
            }
            JobCommands::Cancel { job_id } => {
                job_cancel(&url, &job_id, json).await?;
            }
        },
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

    #[test]
    fn test_session_list_json() {
        let store = new_store();
        let agent = make_agent(&store, "json-agent");
        make_session(&store, agent.id);
        // Just verify it doesn't panic — JSON serialization works
        session_list(&store, None, true).unwrap();
    }

    #[test]
    fn test_session_show_json() {
        let store = new_store();
        let agent = make_agent(&store, "json-show-agent");
        let session = make_session(&store, agent.id);
        // Exercises the as_object_mut().unwrap() path — verifies Session
        // serializes to a JSON object (not array/primitive)
        session_show(&store, &session.id.0.to_string(), true).unwrap();
    }

    #[test]
    fn test_session_delete_json() {
        let store = new_store();
        let agent = make_agent(&store, "json-del-agent");
        let session = make_session(&store, agent.id);
        session_delete(&store, &session.id.0.to_string(), true).unwrap();
        assert!(store.load_session_by_id(session.id).unwrap().is_none());
    }

    // ── Schedule parsing tests ───────────────────────────────────────────

    #[test]
    fn test_parse_schedule_once() {
        let s = parse_schedule("once:2026-03-15T09:00:00Z").unwrap();
        match s {
            JobSchedule::Once { run_at } => {
                assert_eq!(run_at.to_rfc3339(), "2026-03-15T09:00:00+00:00");
            }
            _ => panic!("Expected Once schedule"),
        }
    }

    #[test]
    fn test_parse_schedule_cron() {
        let s = parse_schedule("cron:0 9 * * 1-5").unwrap();
        match s {
            JobSchedule::Recurring { cron } => {
                assert_eq!(cron, "0 9 * * 1-5");
            }
            _ => panic!("Expected Recurring schedule"),
        }
    }

    #[test]
    fn test_parse_schedule_invalid_prefix() {
        let err = parse_schedule("every:5m").unwrap_err();
        assert!(err.to_string().contains("Invalid schedule format"));
    }

    #[test]
    fn test_parse_schedule_invalid_cron_fields() {
        let err = parse_schedule("cron:* *").unwrap_err();
        assert!(err.to_string().contains("Invalid cron expression"));
    }

    #[test]
    fn test_parse_schedule_garbage_cron_rejected() {
        let err = parse_schedule("cron:abc def ghi jkl mno").unwrap_err();
        assert!(err.to_string().contains("Invalid cron expression"));
    }

    #[test]
    fn test_parse_schedule_invalid_once_timestamp() {
        let err = parse_schedule("once:not-a-date").unwrap_err();
        assert!(err.to_string().contains("RFC 3339"));
    }

    // ── Job tests (SQLite-direct) ────────────────────────────────────────

    fn make_job(store: &SqliteStore, agent_id: AgentId, prompt: &str) -> Job {
        let req = CreateJobRequest {
            agent_id,
            prompt: prompt.to_string(),
            schedule: JobSchedule::Once {
                run_at: chrono::Utc::now() + chrono::Duration::hours(1),
            },
        };
        // Use the job_store methods indirectly via store
        let job = Job {
            id: JobId::new(),
            agent_id: req.agent_id,
            prompt: req.prompt,
            schedule: req.schedule,
            status: JobStatus::Pending,
            created_at: chrono::Utc::now(),
            next_run_at: None,
            last_run_at: None,
        };
        store.save_job(&job).unwrap();
        job
    }

    #[test]
    fn test_job_list_empty() {
        let store = new_store();
        job_list(&store, None, false).unwrap();
    }

    #[test]
    fn test_job_list_filters_by_agent() {
        let store = new_store();
        let a1 = make_agent(&store, "job-agent-a");
        let a2 = make_agent(&store, "job-agent-b");
        make_job(&store, a1.id, "prompt A");
        make_job(&store, a1.id, "prompt A2");
        make_job(&store, a2.id, "prompt B");

        let all = store.load_all_jobs_unfiltered().unwrap();
        assert_eq!(all.len(), 3);

        let filtered: Vec<_> = all.into_iter().filter(|j| j.agent_id == a1.id).collect();
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_job_show_found() {
        let store = new_store();
        let agent = make_agent(&store, "job-show-agent");
        let job = make_job(&store, agent.id, "test prompt");
        job_show(&store, &job.id.to_string(), false).unwrap();
    }

    #[test]
    fn test_job_show_not_found() {
        let store = new_store();
        let err = job_show(&store, &uuid::Uuid::new_v4().to_string(), false).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_job_show_invalid_uuid() {
        let store = new_store();
        let err = job_show(&store, "not-a-uuid", false).unwrap_err();
        assert!(err.to_string().contains("Invalid job UUID"));
    }

    #[test]
    fn test_load_job_by_id() {
        let store = new_store();
        let agent = make_agent(&store, "load-job-agent");
        let job = make_job(&store, agent.id, "test");
        let loaded = store.load_job_by_id(job.id).unwrap().unwrap();
        assert_eq!(loaded.prompt, "test");
        assert_eq!(loaded.agent_id, agent.id);
    }

    #[test]
    fn test_load_job_by_id_not_found() {
        let store = new_store();
        assert!(store.load_job_by_id(JobId::new()).unwrap().is_none());
    }

    // ── Completions tests ────────────────────────────────────────────────

    #[test]
    fn test_completions_bash() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(Shell::Bash, &mut cmd, "alms", &mut buf);
        assert!(!buf.is_empty());
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("alms"));
    }

    #[test]
    fn test_completions_powershell() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(Shell::PowerShell, &mut cmd, "alms", &mut buf);
        assert!(!buf.is_empty());
    }

    // ── HTTP helper tests ───────────────────────────────────────────────

    #[test]
    fn test_api_url_basic() {
        assert_eq!(
            api_url("http://localhost:8080", "agents"),
            "http://localhost:8080/agents"
        );
    }

    #[test]
    fn test_api_url_trailing_slash() {
        assert_eq!(
            api_url("http://localhost:8080/", "/agents"),
            "http://localhost:8080/agents"
        );
    }

    #[test]
    fn test_api_url_no_double_slash() {
        assert_eq!(
            api_url("http://localhost:8080/", "agents"),
            "http://localhost:8080/agents"
        );
    }

    #[test]
    fn test_parse_api_error_json() {
        let body = r#"{"error":{"code":"not_found","message":"Agent not found"}}"#;
        let msg = parse_api_error(reqwest::StatusCode::NOT_FOUND, body);
        assert!(msg.contains("Agent not found"));
        assert!(msg.contains("404"));
    }

    #[test]
    fn test_parse_api_error_plain_text() {
        let msg = parse_api_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom");
        assert!(msg.contains("500"));
        assert!(msg.contains("boom"));
    }

    #[test]
    fn test_short_id_normal_uuid() {
        let id = uuid::Uuid::new_v4();
        let s = short_id(&id);
        assert_eq!(s.len(), 8);
    }

    #[test]
    fn test_short_id_short_string() {
        // Verify it doesn't panic on strings shorter than 8
        let s = short_id(&"abc");
        assert_eq!(s, "abc");
    }
}
