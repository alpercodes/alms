// SPDX-License-Identifier: Apache-2.0

use alms_core::CreateJobRequest;
use alms_core::job::{Job, JobId, JobSchedule, JobStatus, JobTerminalReason};
use alms_session::SqliteStore;
use clap::Subcommand;

use crate::helpers::{api_delete, api_get, api_post, fmt_time, resolve_agent, short_id};

#[derive(Subcommand, Debug)]
pub(crate) enum JobCommands {
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

pub(crate) fn job_list(
    store: &SqliteStore,
    agent: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
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
        let status = fmt_job_status(j.status());
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

pub(crate) fn job_show(store: &SqliteStore, job_id_str: &str, json: bool) -> anyhow::Result<()> {
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
    println!("Status:      {}", fmt_job_status(job.status()));
    if let Some(reason) = job.terminal_reason() {
        println!("Reason:      {}", fmt_job_terminal_reason(reason));
    }
    if job.retry_count() > 0 {
        println!("Retries:     {}", job.retry_count());
    }
    if let Some(error) = job.last_error() {
        println!("Last Error:  {error}");
    }
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

pub(crate) async fn job_create(
    client: &reqwest::Client,
    url: &str,
    agent_name_or_id: &str,
    prompt: &str,
    schedule_str: &str,
    json: bool,
) -> anyhow::Result<()> {
    // Resolve agent via the gateway HTTP API instead of direct SQLite,
    // avoiding state disagreement between CLI and gateway. Fixes #26.
    let agent_val = api_get(client, url, &format!("agents/{agent_name_or_id}")).await?;
    let agent_id_str = agent_val
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Agent not found: {agent_name_or_id}"))?;
    let agent_id: alms_core::AgentId = agent_id_str
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid agent ID returned by gateway: {agent_id_str}"))?;
    let agent_name = agent_val
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(agent_name_or_id);

    let schedule = parse_schedule(schedule_str)?;

    let req = CreateJobRequest {
        agent_id,
        prompt: prompt.to_string(),
        schedule,
    };
    let (_status, val) = api_post(client, url, "jobs", &req).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&val)?);
    } else {
        let created: Job = serde_json::from_value(val)?;
        println!("Created job {} for agent '{}'", created.id, agent_name);
    }
    Ok(())
}

pub(crate) async fn job_cancel(
    client: &reqwest::Client,
    url: &str,
    job_id_str: &str,
    json: bool,
) -> anyhow::Result<()> {
    uuid::Uuid::parse_str(job_id_str).map_err(|_| anyhow::anyhow!("Invalid job UUID"))?;
    api_delete(client, url, &format!("jobs/{job_id_str}")).await?;
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
        six_field
            .parse::<cron::Schedule>()
            .map_err(|e| anyhow::anyhow!("Invalid cron expression '{cron_str}': {e}"))?;
        return Ok(JobSchedule::Recurring { cron: cron_str });
    }
    anyhow::bail!("Invalid schedule format. Use 'once:2026-03-15T09:00:00Z' or 'cron:0 9 * * 1-5'");
}

fn fmt_job_status(s: JobStatus) -> &'static str {
    match s {
        JobStatus::Pending => "pending",
        JobStatus::Active => "active",
        JobStatus::Completed => "completed",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
    }
}

fn fmt_job_terminal_reason(reason: JobTerminalReason) -> &'static str {
    match reason {
        JobTerminalReason::Completed => "completed",
        JobTerminalReason::DeadlineReached => "deadline reached",
        JobTerminalReason::RetryExhausted => "retry exhausted",
        JobTerminalReason::OperatorCancelled => "operator cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{make_agent, new_store};
    use alms_core::AgentId;

    fn make_job(store: &SqliteStore, agent_id: AgentId, prompt: &str) -> Job {
        let req = CreateJobRequest {
            agent_id,
            prompt: prompt.to_string(),
            schedule: JobSchedule::Once {
                run_at: chrono::Utc::now() + chrono::Duration::hours(1),
            },
        };
        let job = Job::new(req.agent_id, req.prompt, req.schedule, None);
        store.save_job(&job).unwrap();
        job
    }

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
}
