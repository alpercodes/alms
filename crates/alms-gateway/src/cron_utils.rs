// SPDX-License-Identifier: Apache-2.0

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

/// The job's own next schedule time, **unclamped**.
///
/// - `Once`: returns `run_at` verbatim, even when it is already past due.
/// - `Recurring`: returns the next cron occurrence strictly after `now`.
///
/// Deliberately does not clamp a past-due one-shot into the future. Whether a
/// missed tick fires — and how soon — is `bootstrap_scheduler`'s decision,
/// which staggers the past-due cohort (#1235); a clamp here would hide the
/// past-dueness and let such a job escape the stagger.
pub fn schedule_fire_at(job: &Job, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match &job.schedule {
        JobSchedule::Once { run_at } => Some(*run_at),
        JobSchedule::Recurring { cron } => next_after(cron, now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike as _};

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
    fn test_schedule_fire_at_once_future() {
        let run_at = Utc::now() + chrono::Duration::hours(1);
        let job = make_once_job(run_at);
        assert_eq!(schedule_fire_at(&job, Utc::now()), Some(run_at));
    }

    /// A past-due one-shot reports its real (past) time so the caller can see
    /// that it is a missed tick. `bootstrap_scheduler` owns the catch-up
    /// decision and the #1235 stagger; clamping here would hide it.
    #[test]
    fn test_schedule_fire_at_once_past_due_is_not_clamped() {
        let run_at = Utc::now() - chrono::Duration::hours(1);
        let job = make_once_job(run_at);
        assert_eq!(schedule_fire_at(&job, Utc::now()), Some(run_at));
    }

    #[test]
    fn test_schedule_fire_at_recurring_is_always_future() {
        let now = Utc::now();
        let job = Job::new(
            alms_core::AgentId::new(),
            "test".to_string(),
            JobSchedule::Recurring {
                cron: "0 * * * *".to_string(),
            },
            None,
        );
        assert!(schedule_fire_at(&job, now).unwrap() > now);
    }

    fn make_once_job(run_at: DateTime<Utc>) -> Job {
        Job::new(
            alms_core::AgentId::new(),
            "test".to_string(),
            JobSchedule::Once { run_at },
            None,
        )
    }
}
