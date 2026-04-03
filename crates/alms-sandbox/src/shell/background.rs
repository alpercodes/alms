//! Background task execution for the shell tool.
//!
//! When `run_in_background: true` is specified, the command is spawned as a
//! tokio task and a task ID is returned immediately. The agent can later
//! query the result by task ID.

use super::exec::execute_command;
use super::types::{BackgroundTaskResult, ShellInput, ShellState};
use crate::error::SandboxResult;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};

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
) -> SandboxResult<String> {
    let task_id = state.next_id().await;
    let command_display = input
        .command
        .clone()
        .or_else(|| input.argv.as_ref().map(|a| a.join(" ")))
        .unwrap_or_else(|| "<unknown>".to_string());

    info!(task_id = %task_id, command = %command_display, "Submitting background task");

    let state_clone = state.clone();
    let task_id_clone = task_id.clone();
    let command_clone = command_display.clone();
    let sandbox_root_ref = sandbox_root.clone();

    tokio::spawn(async move {
        let result = execute_command(
            &input,
            &state_clone,
            sandbox_root_ref.as_deref(),
            unrestricted,
            &default_env,
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

/// List all completed background tasks without removing them.
// TODO(dead-code): will be wired to a `list_tasks` action in the shell tool
#[allow(dead_code)]
pub(crate) async fn list_background_tasks(state: &ShellState) -> Vec<(String, bool)> {
    let tasks = state.background_tasks.lock().await;
    tasks
        .iter()
        .map(|(id, result)| {
            let completed = result.output.is_some() || result.error.is_some();
            (id.clone(), completed)
        })
        .collect()
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
            command: Some("echo hello".to_string()),
            argv: None,
            description: None,
            timeout_ms: 5000,
            run_in_background: true,
        };

        let task_id = submit_background_task(input, &state, None, true, HashMap::new())
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
}
