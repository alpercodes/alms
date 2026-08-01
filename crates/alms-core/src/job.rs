//! Job types for scheduled agent runs.

use crate::{
    AgentId,
    lifecycle::{MAX_LIFECYCLE_REVISION, TransitionOutcome},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a scheduled job
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub Uuid);

impl JobId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// When and how often a job runs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobSchedule {
    /// Run once at a specific UTC time
    Once { run_at: DateTime<Utc> },
    /// Run on a 5-field cron expression (e.g. `"0 9 * * 1-5"`)
    Recurring { cron: String },
}

/// Lifecycle status of a job
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Waiting to fire for the first time
    Pending,
    /// Recurring job that has fired at least once
    Active,
    /// Job finished normally and will not fire again
    Completed,
    /// Job exhausted its bounded recovery budget and will not fire again
    Failed,
    /// Cancelled — will not fire again
    Cancelled,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Why a job entered a terminal status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobTerminalReason {
    Completed,
    DeadlineReached,
    RetryExhausted,
    OperatorCancelled,
}

/// The only supported mutations of a job's lifecycle state.
#[derive(Debug, Clone)]
pub enum JobTransition {
    SetNextRunAt(Option<DateTime<Utc>>),
    RecordRecurringRun {
        ran_at: DateTime<Utc>,
        next_run_at: Option<DateTime<Utc>>,
    },
    CompleteOneShot {
        ran_at: DateTime<Utc>,
        terminal_reason: JobTerminalReason,
    },
    RecordDispatchFailure {
        error: String,
        retry_at: DateTime<Utc>,
        max_attempts: u32,
    },
    Cancel,
}

/// A scheduled agent job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub agent_id: AgentId,
    /// Prompt sent to the agent when this job fires
    pub prompt: String,
    pub schedule: JobSchedule,
    status: JobStatus,
    pub created_at: DateTime<Utc>,
    /// Next scheduled fire time (`None` for recurring until the scheduler computes it)
    pub next_run_at: Option<DateTime<Utc>>,
    /// Last time this job fired successfully
    pub last_run_at: Option<DateTime<Utc>>,
    /// Monotonically increases for every accepted lifecycle mutation.
    #[serde(default)]
    lifecycle_revision: u64,
    /// Refines a terminal status with a machine-readable cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_reason: Option<JobTerminalReason>,
    /// Consecutive recoverable dispatch failures since the last successful run.
    #[serde(default)]
    retry_count: u32,
    /// Most recent recoverable dispatch error, retained while retrying or when
    /// the retry budget is exhausted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

impl Job {
    pub fn new(
        agent_id: AgentId,
        prompt: String,
        schedule: JobSchedule,
        next_run_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            id: JobId::new(),
            agent_id,
            prompt,
            schedule,
            status: JobStatus::Pending,
            created_at: Utc::now(),
            next_run_at,
            last_run_at: None,
            lifecycle_revision: 0,
            terminal_reason: None,
            retry_count: 0,
            last_error: None,
        }
    }

    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn from_persisted(
        id: JobId,
        agent_id: AgentId,
        prompt: String,
        schedule: JobSchedule,
        status: JobStatus,
        created_at: DateTime<Utc>,
        next_run_at: Option<DateTime<Utc>>,
        last_run_at: Option<DateTime<Utc>>,
        lifecycle_revision: u64,
        terminal_reason: Option<JobTerminalReason>,
        retry_count: u32,
        last_error: Option<String>,
    ) -> Self {
        Self {
            id,
            agent_id,
            prompt,
            schedule,
            status,
            created_at,
            next_run_at,
            last_run_at,
            lifecycle_revision,
            terminal_reason,
            retry_count,
            last_error,
        }
    }

    pub fn status(&self) -> JobStatus {
        self.status
    }

    pub fn lifecycle_revision(&self) -> u64 {
        self.lifecycle_revision
    }

    pub fn terminal_reason(&self) -> Option<JobTerminalReason> {
        self.terminal_reason
    }

    pub fn retry_count(&self) -> u32 {
        self.retry_count
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Apply a lifecycle transition under the caller's synchronization
    /// boundary. Terminal states are absorbing and duplicate cancellation is
    /// a no-op, so competing scheduler/operator writers have one winner.
    pub fn transition(&mut self, transition: JobTransition) -> TransitionOutcome<JobStatus> {
        let from = self.status;
        if from.is_terminal() {
            return match transition {
                JobTransition::Cancel if from == JobStatus::Cancelled => TransitionOutcome::NoOp {
                    state: from,
                    revision: self.lifecycle_revision,
                },
                _ => TransitionOutcome::Rejected {
                    from,
                    to: match &transition {
                        JobTransition::RecordRecurringRun { .. } => JobStatus::Active,
                        JobTransition::CompleteOneShot { .. } => JobStatus::Completed,
                        JobTransition::RecordDispatchFailure { .. } => JobStatus::Failed,
                        JobTransition::SetNextRunAt(_) => from,
                        JobTransition::Cancel => JobStatus::Cancelled,
                    },
                    revision: self.lifecycle_revision,
                },
            };
        }
        if let JobTransition::SetNextRunAt(next) = &transition
            && self.next_run_at == *next
        {
            return TransitionOutcome::NoOp {
                state: from,
                revision: self.lifecycle_revision,
            };
        }

        let to = match &transition {
            JobTransition::SetNextRunAt(_) => from,
            JobTransition::RecordRecurringRun { .. } => JobStatus::Active,
            JobTransition::CompleteOneShot { .. } => JobStatus::Completed,
            JobTransition::RecordDispatchFailure { max_attempts, .. } => {
                if self.retry_count.saturating_add(1) >= *max_attempts {
                    JobStatus::Failed
                } else {
                    from
                }
            }
            JobTransition::Cancel => JobStatus::Cancelled,
        };
        let legal = match (&transition, &self.schedule) {
            (JobTransition::SetNextRunAt(_), _) | (JobTransition::Cancel, _) => true,
            (JobTransition::RecordRecurringRun { .. }, JobSchedule::Recurring { .. }) => true,
            (JobTransition::RecordDispatchFailure { max_attempts, .. }, _) => *max_attempts > 0,
            (
                JobTransition::CompleteOneShot {
                    terminal_reason, ..
                },
                JobSchedule::Once { .. },
            ) => {
                matches!(
                    terminal_reason,
                    JobTerminalReason::Completed | JobTerminalReason::DeadlineReached
                )
            }
            _ => false,
        };
        if !legal || self.lifecycle_revision >= MAX_LIFECYCLE_REVISION {
            return TransitionOutcome::Rejected {
                from,
                to,
                revision: self.lifecycle_revision,
            };
        }

        let to = match transition {
            JobTransition::SetNextRunAt(next) => {
                self.next_run_at = next;
                from
            }
            JobTransition::RecordRecurringRun {
                ran_at,
                next_run_at,
            } => {
                self.last_run_at = Some(ran_at);
                self.status = JobStatus::Active;
                self.next_run_at = next_run_at;
                self.terminal_reason = None;
                self.retry_count = 0;
                self.last_error = None;
                JobStatus::Active
            }
            JobTransition::CompleteOneShot {
                ran_at,
                terminal_reason,
            } => {
                self.last_run_at = Some(ran_at);
                self.status = JobStatus::Completed;
                self.next_run_at = None;
                self.terminal_reason = Some(terminal_reason);
                self.retry_count = 0;
                self.last_error = None;
                JobStatus::Completed
            }
            JobTransition::RecordDispatchFailure {
                error,
                retry_at,
                max_attempts,
            } => {
                self.retry_count = self.retry_count.saturating_add(1);
                self.last_error = Some(error);
                if self.retry_count >= max_attempts {
                    self.status = JobStatus::Failed;
                    self.next_run_at = None;
                    self.terminal_reason = Some(JobTerminalReason::RetryExhausted);
                    JobStatus::Failed
                } else {
                    self.next_run_at = Some(retry_at);
                    self.terminal_reason = None;
                    from
                }
            }
            JobTransition::Cancel => {
                self.status = JobStatus::Cancelled;
                self.next_run_at = None;
                self.terminal_reason = Some(JobTerminalReason::OperatorCancelled);
                self.retry_count = 0;
                self.last_error = None;
                JobStatus::Cancelled
            }
        };

        self.lifecycle_revision += 1;
        TransitionOutcome::Applied {
            from,
            to,
            revision: self.lifecycle_revision,
        }
    }
}

/// Request body for `POST /jobs`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateJobRequest {
    pub agent_id: AgentId,
    pub prompt: String,
    pub schedule: JobSchedule,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(schedule: JobSchedule) -> Job {
        Job {
            id: JobId::new(),
            agent_id: AgentId::new(),
            prompt: "test".to_string(),
            schedule,
            status: JobStatus::Pending,
            created_at: Utc::now(),
            next_run_at: None,
            last_run_at: None,
            lifecycle_revision: 0,
            terminal_reason: None,
            retry_count: 0,
            last_error: None,
        }
    }

    #[test]
    fn test_create_job_request_recurring_from_ui_json() {
        let json = r#"{
            "agent_id": "a1b2c3d4-e5f6-4789-abcd-ef0123456789",
            "schedule": { "type": "recurring", "cron": "*/5 * * * *" },
            "prompt": "test prompt"
        }"#;
        let req: CreateJobRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.prompt, "test prompt");
        match req.schedule {
            JobSchedule::Recurring { ref cron } => assert_eq!(cron, "*/5 * * * *"),
            _ => panic!("Expected Recurring schedule"),
        }
    }

    #[test]
    fn test_create_job_request_once_from_ui_json() {
        let json = r#"{
            "agent_id": "a1b2c3d4-e5f6-4789-abcd-ef0123456789",
            "schedule": { "type": "once", "run_at": "2026-03-23T15:30:00.000Z" },
            "prompt": "one-time task"
        }"#;
        let req: CreateJobRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.prompt, "one-time task");
        match req.schedule {
            JobSchedule::Once { run_at } => {
                assert_eq!(run_at.to_rfc3339(), "2026-03-23T15:30:00+00:00");
            }
            _ => panic!("Expected Once schedule"),
        }
    }

    #[test]
    fn job_transitions_validate_schedule_semantics() {
        let mut once = job(JobSchedule::Once { run_at: Utc::now() });
        assert!(matches!(
            once.transition(JobTransition::RecordRecurringRun {
                ran_at: Utc::now(),
                next_run_at: Some(Utc::now()),
            }),
            TransitionOutcome::Rejected { .. }
        ));
        assert_eq!(once.status, JobStatus::Pending);
        assert_eq!(once.lifecycle_revision, 0);

        let mut recurring = job(JobSchedule::Recurring {
            cron: "* * * * *".to_string(),
        });
        assert!(matches!(
            recurring.transition(JobTransition::CompleteOneShot {
                ran_at: Utc::now(),
                terminal_reason: JobTerminalReason::Completed,
            }),
            TransitionOutcome::Rejected { .. }
        ));
        assert_eq!(recurring.status, JobStatus::Pending);
        assert_eq!(recurring.lifecycle_revision, 0);
    }

    #[test]
    fn job_revision_exhaustion_is_rejected_without_mutation() {
        let mut job = job(JobSchedule::Recurring {
            cron: "* * * * *".to_string(),
        });
        job.lifecycle_revision = MAX_LIFECYCLE_REVISION;
        assert!(matches!(
            job.transition(JobTransition::RecordRecurringRun {
                ran_at: Utc::now(),
                next_run_at: Some(Utc::now()),
            }),
            TransitionOutcome::Rejected { .. }
        ));
        assert_eq!(job.status, JobStatus::Pending);
        assert!(job.last_run_at.is_none());
        assert_eq!(job.lifecycle_revision, MAX_LIFECYCLE_REVISION);
    }

    #[test]
    fn dispatch_failures_are_bounded_and_terminal() {
        let mut job = job(JobSchedule::Once { run_at: Utc::now() });
        let first_retry = Utc::now() + chrono::Duration::seconds(5);
        assert!(matches!(
            job.transition(JobTransition::RecordDispatchFailure {
                error: "first".to_string(),
                retry_at: first_retry,
                max_attempts: 3,
            }),
            TransitionOutcome::Applied {
                to: JobStatus::Pending,
                ..
            }
        ));
        assert_eq!(job.retry_count(), 1);
        assert_eq!(job.next_run_at, Some(first_retry));
        assert_eq!(job.last_error(), Some("first"));

        assert!(matches!(
            job.transition(JobTransition::RecordDispatchFailure {
                error: "second".to_string(),
                retry_at: Utc::now(),
                max_attempts: 3,
            }),
            TransitionOutcome::Applied {
                to: JobStatus::Pending,
                ..
            }
        ));
        assert_eq!(job.retry_count(), 2);

        assert!(matches!(
            job.transition(JobTransition::RecordDispatchFailure {
                error: "third".to_string(),
                retry_at: Utc::now(),
                max_attempts: 3,
            }),
            TransitionOutcome::Applied {
                to: JobStatus::Failed,
                ..
            }
        ));
        assert_eq!(job.status(), JobStatus::Failed);
        assert_eq!(job.retry_count(), 3);
        assert_eq!(
            job.terminal_reason(),
            Some(JobTerminalReason::RetryExhausted)
        );
        assert_eq!(job.last_error(), Some("third"));
        assert!(matches!(
            job.transition(JobTransition::SetNextRunAt(Some(Utc::now()))),
            TransitionOutcome::Rejected { .. }
        ));
    }

    #[test]
    fn successful_recurring_run_clears_retry_metadata() {
        let mut job = job(JobSchedule::Recurring {
            cron: "* * * * *".to_string(),
        });
        assert!(
            job.transition(JobTransition::RecordDispatchFailure {
                error: "temporary".to_string(),
                retry_at: Utc::now(),
                max_attempts: 3,
            })
            .is_applied()
        );
        assert!(
            job.transition(JobTransition::RecordRecurringRun {
                ran_at: Utc::now(),
                next_run_at: Some(Utc::now()),
            })
            .is_applied()
        );
        assert_eq!(job.retry_count(), 0);
        assert_eq!(job.last_error(), None);
    }
}
