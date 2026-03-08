//! Cron expression utilities for scheduled job management.
//!
//! Wraps the `cron` crate (6-field: sec min hour dom month dow) and adapts it
//! for the ALMS 5-field standard (min hour dom month dow) by prepending sec=0.

use alms_core::job::{Job, JobSchedule};
use chrono::{DateTime, Utc};
use cron::Schedule;
use std::str::FromStr;

/// Parse a 5-field cron expression and return the next fire time strictly after `after`.
///
/// The 5-field format is: `min hour dom month dow` (standard crontab).
/// Returns `None` if the expression is invalid or has no future occurrences.
pub fn next_after(expr: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    // Convert 5-field → 6-field by prepending sec=0.
    let six_field = format!("0 {expr}");
    let schedule = Schedule::from_str(&six_field).ok()?;
    schedule.after(&after).next()
}

/// Compute the next fire time for a job given the current time.
///
/// - `Once`: returns `run_at`, clamped 1 second into the future if past-due
///   (so missed jobs fire almost immediately on restart).
/// - `Recurring`: returns the next cron occurrence after `now`.
pub fn compute_next_fire(job: &Job, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match &job.schedule {
        JobSchedule::Once { run_at } => {
            let t = if *run_at <= now {
                now + chrono::Duration::seconds(1)
            } else {
                *run_at
            };
            Some(t)
        }
        JobSchedule::Recurring { cron } => next_after(cron, now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_next_after_valid_expr() {
        // "0 9 * * 1-5" = 09:00 Mon-Fri
        let base = Utc.with_ymd_and_hms(2026, 3, 9, 8, 0, 0).unwrap(); // Monday 08:00
        let next = next_after("0 9 * * 1-5", base).unwrap();
        assert_eq!(next.hour(), 9);
    }

    #[test]
    fn test_next_after_invalid_expr() {
        assert!(next_after("not a cron expression", Utc::now()).is_none());
    }

    #[test]
    fn test_compute_next_fire_once_future() {
        let run_at = Utc::now() + chrono::Duration::hours(1);
        let job = make_once_job(run_at);
        let next = compute_next_fire(&job, Utc::now()).unwrap();
        assert_eq!(next, run_at);
    }

    #[test]
    fn test_compute_next_fire_once_past_due() {
        let run_at = Utc::now() - chrono::Duration::hours(1);
        let job = make_once_job(run_at);
        let now = Utc::now();
        let next = compute_next_fire(&job, now).unwrap();
        // Should be ~1s in the future, not the past run_at
        assert!(next > now);
        assert!(next <= now + chrono::Duration::seconds(2));
    }

    fn make_once_job(run_at: DateTime<Utc>) -> Job {
        use alms_core::{
            AgentId,
            job::{JobId, JobStatus},
        };
        Job {
            id: JobId::new(),
            agent_id: AgentId::new(),
            prompt: "test".to_string(),
            schedule: JobSchedule::Once { run_at },
            status: JobStatus::Pending,
            created_at: Utc::now(),
            next_run_at: None,
            last_run_at: None,
        }
    }
}

// Re-export chrono::Timelike for the hour() call in tests
#[cfg(test)]
use chrono::Timelike as _;
