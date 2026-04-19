//! Background task execution for the shell tool.
//!
//! When `run_in_background: true` is specified, the command is spawned as a
//! tokio task and a task ID is returned immediately. The agent can later
//! query the result by task ID.

use super::exec::execute_command;
use super::spill::ShellSpillPolicy;
use super::types::{BackgroundTaskResult, ShellInput, ShellState};
use crate::error::SandboxResult;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Maximum number of completed background task results to retain.
/// When exceeded, the oldest entries are evicted to prevent memory leaks.
const MAX_COMPLETED_TASKS: usize = 100;

/// Submit a command for background execution.
///
/// Returns the task ID immediately. The command runs in a tokio spawned task
/// and its result is stored in the shell state's `background_tasks` map.
pub(crate) async fn submit_background_task(
    input: ShellInput,
    state: &ShellState,
    sandbox_root: Option<PathBuf>,
    unrestricted: bool,
    default_env: HashMap<String, String>,
    pwd_marker: String,
    spill_policy: ShellSpillPolicy,
) -> SandboxResult<String> {
    let task_id = state.next_id().await;
    let command_display = input.command.clone();

    info!(task_id = %task_id, command = %command_display, "Submitting background task");

    let state_clone = state.clone();
    let task_id_clone = task_id.clone();
    let command_clone = command_display.clone();
    let sandbox_root_ref = sandbox_root.clone();
    // Use the generated background task id as the spill tool_call_id so
    // spill files are grep-able against the ShellTool's task_id response.
    let spill_tool_call_id = task_id.clone();

    tokio::spawn(async move {
        let result = execute_command(
            &input,
            &state_clone,
            sandbox_root_ref.as_deref(),
            unrestricted,
            &default_env,
            &pwd_marker,
            &spill_policy,
            &spill_tool_call_id,
        )
        .await;

        let task_result = match result {
            Ok(output) => {
                debug!(task_id = %task_id_clone, exit_code = output.exit_code, "Background task completed");
                BackgroundTaskResult {
                    task_id: task_id_clone.clone(),
                    command: command_clone,
                    output: Some(output),
                    error: None,
                }
            }
            Err(e) => {
                warn!(task_id = %task_id_clone, error = %e, "Background task failed");
                BackgroundTaskResult {
                    task_id: task_id_clone.clone(),
                    command: command_clone,
                    output: None,
                    error: Some(e.to_string()),
                }
            }
        };

        let mut tasks = state_clone.background_tasks.lock().await;
        // Evict oldest entries when the map exceeds the limit to prevent
        // unbounded memory growth from long-running agents.
        if tasks.len() >= MAX_COMPLETED_TASKS {
            evict_oldest(&mut tasks);
        }
        tasks.insert(task_id_clone, task_result);
    });

    Ok(task_id)
}

/// Check the result of a background task.
///
/// Returns `Some(result)` if the task has completed, `None` if still running.
/// Completed tasks are removed from the map after retrieval.
pub(crate) async fn check_background_task(
    state: &ShellState,
    task_id: &str,
) -> Option<BackgroundTaskResult> {
    let mut tasks = state.background_tasks.lock().await;
    tasks.remove(task_id)
}

/// Evict the oldest completed task from the map.
///
/// Task IDs are formatted as `bg_N` where N is a monotonically increasing
/// counter, so the lexicographically smallest ID is the oldest. We parse
/// the numeric suffix for correct ordering.
fn evict_oldest(tasks: &mut HashMap<String, BackgroundTaskResult>) {
    if let Some(oldest_key) = tasks
        .keys()
        .min_by_key(|k| {
            k.strip_prefix("bg_")
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(u64::MAX)
        })
        .cloned()
    {
        debug!(task_id = %oldest_key, "Evicting oldest background task result");
        tasks.remove(&oldest_key);
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::ShellState;
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_submit_and_check_background_task() {
        let state = ShellState::new(PathBuf::from("."));
        let input = ShellInput {
            command: "echo hello".to_string(),
            description: None,
            timeout_ms: 5000,
            run_in_background: true,
        };

        let task_id = submit_background_task(
            input,
            &state,
            None,
            true,
            HashMap::new(),
            "__ALMS_PWD_TEST__".to_string(),
            ShellSpillPolicy::disabled(),
        )
        .await
        .unwrap();

        assert!(task_id.starts_with("bg_"));

        // Wait a bit for the task to complete
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Check the result
        let result = check_background_task(&state, &task_id).await;
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.task_id, task_id);
        assert!(result.output.is_some());
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn test_check_nonexistent_task() {
        let state = ShellState::new(PathBuf::from("."));
        let result = check_background_task(&state, "bg_999").await;
        assert!(result.is_none());
    }

    #[test]
    fn test_evict_oldest_removes_lowest_id() {
        let mut tasks = HashMap::new();
        for i in [5, 2, 8, 1, 3] {
            tasks.insert(
                format!("bg_{i}"),
                BackgroundTaskResult {
                    task_id: format!("bg_{i}"),
                    command: "test".to_string(),
                    output: None,
                    error: Some("done".to_string()),
                },
            );
        }
        assert_eq!(tasks.len(), 5);
        evict_oldest(&mut tasks);
        assert_eq!(tasks.len(), 4);
        assert!(
            !tasks.contains_key("bg_1"),
            "bg_1 should have been evicted as the oldest"
        );
    }

    #[test]
    fn test_evict_oldest_empty_map_is_noop() {
        let mut tasks: HashMap<String, BackgroundTaskResult> = HashMap::new();
        evict_oldest(&mut tasks);
        assert!(tasks.is_empty());
    }
}
