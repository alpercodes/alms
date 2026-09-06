// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::llm_client::LlmClient;
use crate::tools::ToolRegistry;
use alms_core::AgentId;

mod allow_full_os_access;
mod approval;
mod cancel_during_tool;
mod config;
mod context;
mod context_debug;
mod dm;
mod dm_conflict;
mod episodic;
mod fs_read_roots_accumulator;
mod loop_limits;
mod persistence;
mod sandbox;
mod tool_output_truncate_integration;
mod types;
mod usage;
mod workspace_view;

/// Baseline `AgentRuntime` for tests: the given `llm`, default
/// `AgentConfig`, an empty tool registry, no workspace, no event sender,
/// no cancel token, an unrestricted shell with default permissions and
/// classification, and every spill / truncation policy disabled.
///
/// Tests override the fields they care about with struct-update syntax —
/// `AgentRuntime { event_sender: Some(tx), ..base_runtime(llm) }` — so a
/// new `AgentRuntime` field means one edit here, not one per test.
fn base_runtime(llm: LlmClient) -> AgentRuntime {
    AgentRuntime {
        agent_id: AgentId::new(),
        config: AgentConfig::default(),
        llm,
        summary_llm: None,
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: None,
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    }
}
