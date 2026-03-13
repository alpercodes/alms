use alms_core::{CreateRunRequest, CreateRunResponse, RunInput, RunStatusResponse, SessionId};
use clap::Subcommand;

use crate::helpers::{api_get, api_post, fmt_time, short_id};

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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_create(
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

pub(crate) async fn run_list(
    url: &str,
    session: &str,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
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

pub(crate) async fn run_show(url: &str, run_id: &str, json: bool) -> anyhow::Result<()> {
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
