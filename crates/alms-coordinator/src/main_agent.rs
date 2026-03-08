//! Main Agent implementation
//!
//! The Main Agent is the user's primary interface. It:
//! 1. Receives user input
//! 2. Understands intent
//! 3. Decomposes complex tasks
//! 4. Spawns subagents for parallel execution
//! 5. Synthesizes results into coherent responses

#![allow(dead_code)]

use crate::{Coordinator, SubagentRequest, SubagentType, TaskId, TaskResult, TaskStatus};
use alms_core::{AgentId, AlmsResult};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Main Agent handles user interactions and orchestrates subagents
#[derive(Debug)]
pub struct MainAgent {
    agent_id: AgentId,
    coordinator: Coordinator,
    /// Track pending subagent tasks
    pending_tasks: HashMap<TaskId, SubagentRequest>,
    /// Results collected from subagents
    task_results: HashMap<TaskId, TaskResult>,
}

impl MainAgent {
    pub fn new(agent_id: AgentId) -> Self {
        let coordinator = Coordinator::new(agent_id);

        Self {
            agent_id,
            coordinator,
            pending_tasks: HashMap::new(),
            task_results: HashMap::new(),
        }
    }

    /// Process user input - the main entry point
    pub async fn process_input(&mut self, input: &str) -> AlmsResult<String> {
        info!("Main Agent processing: {}", input);

        // Step 1: Analyze intent and decompose task
        let subtasks = self.decompose_task(input).await?;

        if subtasks.is_empty() {
            // Simple task - handle directly
            return Ok(format!("Processed: {}", input));
        }

        // Step 2: Spawn subagents for each subtask
        let mut task_ids = Vec::new();
        for subtask in subtasks {
            let task_id = self.spawn_subagent_for_task(subtask).await?;
            task_ids.push(task_id);
        }

        // Step 3: Collect results from all subagents
        self.collect_results(&task_ids).await?;

        // Step 4: Synthesize final response
        let response = self.synthesize_response(input, &task_ids).await?;

        // Cleanup
        for task_id in task_ids {
            self.pending_tasks.remove(&task_id);
            self.task_results.remove(&task_id);
        }

        Ok(response)
    }

    /// Analyze input and decompose into subtasks
    async fn decompose_task(&self, input: &str) -> AlmsResult<Vec<SubagentRequest>> {
        // Simple rule-based decomposition (replace with LLM-based planning)
        let input_lower = input.to_lowercase();

        if input_lower.contains("research") || input_lower.contains("find") {
            Ok(vec![SubagentRequest {
                task: input.to_string(),
                agent_type: SubagentType::Research,
                timeout: Duration::from_secs(300),
                capabilities: SubagentType::Research.default_capabilities(),
                parent_session: alms_core::SessionId::new(),
                parent_run_id: None,
            }])
        } else if input_lower.contains("code")
            || input_lower.contains("build")
            || input_lower.contains("implement")
        {
            Ok(vec![SubagentRequest {
                task: input.to_string(),
                agent_type: SubagentType::Code,
                timeout: Duration::from_secs(600),
                capabilities: SubagentType::Code.default_capabilities(),
                parent_session: alms_core::SessionId::new(),
                parent_run_id: None,
            }])
        } else if input_lower.contains("analyze") || input_lower.contains("data") {
            Ok(vec![SubagentRequest {
                task: input.to_string(),
                agent_type: SubagentType::Data,
                timeout: Duration::from_secs(300),
                capabilities: SubagentType::Data.default_capabilities(),
                parent_session: alms_core::SessionId::new(),
                parent_run_id: None,
            }])
        } else if input_lower.contains("security") || input_lower.contains("audit") {
            Ok(vec![SubagentRequest {
                task: input.to_string(),
                agent_type: SubagentType::Security,
                timeout: Duration::from_secs(300),
                capabilities: SubagentType::Security.default_capabilities(),
                parent_session: alms_core::SessionId::new(),
                parent_run_id: None,
            }])
        } else {
            // Complex task - decompose into multiple subtasks
            if input_lower.contains("and") || input_lower.contains("then") {
                Ok(vec![
                    SubagentRequest {
                        task: format!("Research phase of: {}", input),
                        agent_type: SubagentType::Research,
                        timeout: Duration::from_secs(300),
                        capabilities: SubagentType::Research.default_capabilities(),
                        parent_session: alms_core::SessionId::new(),
                        parent_run_id: None,
                    },
                    SubagentRequest {
                        task: format!("Implementation phase of: {}", input),
                        agent_type: SubagentType::Code,
                        timeout: Duration::from_secs(600),
                        capabilities: SubagentType::Code.default_capabilities(),
                        parent_session: alms_core::SessionId::new(),
                        parent_run_id: None,
                    },
                ])
            } else {
                // Single general-purpose subagent
                Ok(vec![SubagentRequest {
                    task: input.to_string(),
                    agent_type: SubagentType::General,
                    timeout: Duration::from_secs(300),
                    capabilities: SubagentType::General.default_capabilities(),
                    parent_session: alms_core::SessionId::new(),
                    parent_run_id: None,
                }])
            }
        }
    }

    /// Spawn a subagent for a task
    async fn spawn_subagent_for_task(&mut self, request: SubagentRequest) -> AlmsResult<TaskId> {
        let task_id = self.coordinator.spawn_subagent(request.clone()).await?;
        self.pending_tasks.insert(task_id, request);
        debug!("Spawned subagent {:?} for task", task_id);
        Ok(task_id)
    }

    /// Collect results from subagents
    async fn collect_results(&mut self, task_ids: &[TaskId]) -> AlmsResult<()> {
        // In real implementation, this would listen on a channel
        // For now, poll status until all complete
        let mut completed = 0;
        let max_wait = Duration::from_secs(60);
        let start = std::time::Instant::now();

        while completed < task_ids.len() && start.elapsed() < max_wait {
            for task_id in task_ids {
                if self.task_results.contains_key(task_id) {
                    continue; // Already collected
                }

                if let Some(status) = self.coordinator.get_status(*task_id) {
                    match status {
                        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled => {
                            // In real impl, would get actual result from channel
                            self.task_results.insert(
                                *task_id,
                                TaskResult {
                                    task_id: *task_id,
                                    status,
                                    result: serde_json::json!({"status": format!("{:?}", status)}),
                                    execution_time_ms: 1000,
                                    tokens_used: Some(500),
                                },
                            );
                            completed += 1;
                        }
                        _ => {}
                    }
                }
            }

            if completed < task_ids.len() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        if completed < task_ids.len() {
            warn!(
                "Timeout waiting for {} subagents",
                task_ids.len() - completed
            );
        }

        Ok(())
    }

    /// Synthesize final response from subagent results
    async fn synthesize_response(
        &self,
        original_input: &str,
        task_ids: &[TaskId],
    ) -> AlmsResult<String> {
        let mut parts = vec![format!("Based on your request: \"{}\"", original_input)];

        for task_id in task_ids {
            if let Some(request) = self.pending_tasks.get(task_id) {
                parts.push(format!("\n**{:?} Subagent**:", request.agent_type));

                if let Some(result) = self.task_results.get(task_id) {
                    match result.status {
                        TaskStatus::Completed => {
                            parts.push(format!("✓ Completed in {}ms", result.execution_time_ms));
                            parts.push(format!("Result: {}", result.result));
                        }
                        TaskStatus::Failed => {
                            parts.push(format!("✗ Failed: {}", result.result));
                        }
                        TaskStatus::Cancelled => {
                            parts.push("✗ Cancelled".to_string());
                        }
                        _ => {
                            parts.push("⏳ In progress...".to_string());
                        }
                    }
                } else {
                    parts.push("⏳ No result yet".to_string());
                }
            }
        }

        Ok(parts.join("\n"))
    }

    /// Cancel all running subagents
    pub fn cancel_all(&self) {
        let active = self.coordinator.list_active();
        let count = active.len();
        for (task_id, _, _) in active {
            let _ = self.coordinator.cancel_subagent(task_id);
        }
        info!("Cancelled {} subagents", count);
    }

    /// Get coordinator for advanced operations
    pub fn coordinator(&self) -> &Coordinator {
        &self.coordinator
    }
}
