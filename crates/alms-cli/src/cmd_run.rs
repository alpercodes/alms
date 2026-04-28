use alms_core::{CreateRunRequest, CreateRunResponse, RunInput, RunStatusResponse, SessionId};
use clap::Subcommand;

use crate::helpers::{api_get, api_post, fmt_time, open_db, resolve_agent, short_id};

#[derive(Subcommand, Debug)]
pub(crate) enum RunCommands {
    /// Create a new run (requires running gateway)
    Create {
        /// Session UUID
        #[arg(long)]
        session: String,
        /// Input text for the agent
        #[arg(long)]
        input: String,
        /// Agent name or UUID. Required when creating a run on a shared DM session.
        #[arg(long)]
        agent: Option<String>,
        /// Model override
        #[arg(long)]
        model: Option<String>,
        /// Max tokens override
        #[arg(long)]
        max_tokens: Option<u32>,
        /// Posture override ("guarded", "full_control", or "autonomous")
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_create(
    client: &reqwest::Client,
    url: &str,
    session: &str,
    input: &str,
    agent: Option<String>,
    model: Option<String>,
    max_tokens: Option<u32>,
    posture: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let session_uuid =
        uuid::Uuid::parse_str(session).map_err(|_| anyhow::anyhow!("Invalid session UUID"))?;
    let agent_id = if let Some(agent) = agent {
        let store = open_db()?;
        Some(resolve_agent(&store, &agent)?.id)
    } else {
        None
    };
    let req = CreateRunRequest {
        session_id: SessionId(session_uuid),
        agent_id,
        input: RunInput::Text {
            text: input.to_string(),
        },
        model,
        max_tokens,
        posture,
        provider: None,
        debug_mode: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
    };
    let (_status, val) = api_post(client, url, "runs", &req).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&val)?);
    } else {
        let resp: CreateRunResponse = serde_json::from_value(val)?;
        println!("Created run {} (status: {:?})", resp.run_id.0, resp.status);
    }
    Ok(())
}

pub(crate) async fn run_list(
    client: &reqwest::Client,
    url: &str,
    session: &str,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    let _uuid =
        uuid::Uuid::parse_str(session).map_err(|_| anyhow::anyhow!("Invalid session UUID"))?;
    let path = format!("runs?session_id={session}&limit={limit}");
    let val = api_get(client, url, &path).await?;

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

pub(crate) async fn run_show(
    client: &reqwest::Client,
    url: &str,
    run_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let _uuid = uuid::Uuid::parse_str(run_id).map_err(|_| anyhow::anyhow!("Invalid run UUID"))?;
    let val = api_get(client, url, &format!("runs/{run_id}")).await?;

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
