// SPDX-License-Identifier: Apache-2.0

// ── #851 — In-loop tool-output truncation integration ───────────────────────

use super::base_runtime;
use crate::agent::*;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use crate::tool_output_truncate::ToolOutputTruncatePolicy;
use alms_core::AgentId;
use alms_session::{SessionConfig, SessionManager};

/// Build a minimal `AgentRuntime` with the in-loop truncation service
/// active and pointed at `run_dir`. No LLM or workspace — we are
/// driving `process_tool_results` directly.
fn make_runtime_with_truncate(run_dir: std::path::PathBuf) -> AgentRuntime {
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let mut policy = ToolOutputTruncatePolicy::with_run_dir(run_dir);
    // Use small caps so the test stays fast and the assertions are
    // concrete (32 KB is too big to construct quickly in a unit test
    // and offers no extra coverage).
    policy.max_bytes = 4 * 1024;
    policy.max_lines = 100;
    AgentRuntime {
        tool_output_truncate_policy: policy,
        ..base_runtime(LlmClient::new(llm_config).unwrap())
    }
}

/// Regression for the subagent-shaped builder path (#921): constructing
/// the runtime through `AgentRuntime::new(...).with_tool_output_truncate(...)`
/// must activate the policy, place spill files under the supplied
/// `tool-output/sub-{task_id}/` directory, bound both the in-loop and
/// audit/SSE previews, and persist the truncation metadata used when the
/// session is reconstructed on a later run.
#[tokio::test]
async fn builder_shaped_subagent_runtime_truncates_oversized_tool_output() {
    let data_dir = tempfile::tempdir().unwrap();
    let task_id = uuid::Uuid::new_v4();
    let sub_run_dir = data_dir
        .path()
        .join(crate::tool_output_truncate::TOOL_OUTPUT_DIR_NAME)
        .join(format!("sub-{task_id}"));

    let config = AgentConfig {
        tool_output_truncate: alms_core::config::ToolOutputTruncateConfig {
            enabled: true,
            max_bytes: 4 * 1024,
            max_lines: 100,
            retention_days: 7,
        },
        ..AgentConfig::default()
    };
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let runtime = AgentRuntime::new(AgentId::new(), config, LlmClient::new(llm_config).unwrap())
        .unwrap()
        .with_tool_output_truncate(sub_run_dir.clone(), true, 4 * 1024, 100);

    let session_manager = SessionManager::new(SessionConfig::default());
    let session = session_manager.get_or_create(AgentId::new(), "subagent-test");
    let tool_call = ToolCall::new("call_subagent_huge", "fake_tool", "{}");
    let result_value = serde_json::Value::String("x".repeat(100 * 1024));
    let mut messages = Vec::new();
    let mut records = Vec::new();
    let mut seq = 0;
    let invocation_ids = vec![uuid::Uuid::new_v4()];

    runtime.process_tool_results(
        std::slice::from_ref(&tool_call),
        vec![Ok(result_value.clone())],
        &invocation_ids,
        &mut messages,
        &mut records,
        &mut seq,
        &session_manager,
        session.id,
        false,
    );

    let spill_path = sub_run_dir.join("tool_call_subagent_huge.txt");
    assert!(
        spill_path.exists(),
        "subagent spill must land under tool-output/sub-{}/: {}",
        task_id,
        spill_path.display()
    );
    assert_eq!(
        std::fs::read_to_string(&spill_path).unwrap(),
        result_value.to_string(),
        "spill bytes must match the original JSON-stringified result"
    );

    assert_eq!(messages.len(), 1);
    let preview = messages[0].content_str();
    assert!(
        preview.len() < 8 * 1024,
        "subagent in-loop preview must be capped: {} bytes",
        preview.len()
    );

    let emit_value = runtime.truncate_for_emit(&tool_call.id, &result_value);
    let emit_preview = emit_value
        .as_str()
        .expect("truncated emit value must be a JSON string");
    assert!(
        emit_preview.len() < 8 * 1024,
        "subagent audit/SSE preview must be capped: {} bytes",
        emit_preview.len()
    );

    let history = session_manager.get_history(session.id).unwrap();
    let tool_msg = history
        .iter()
        .find(|message| matches!(message.role, alms_session::Role::Tool))
        .expect("subagent tool result must be persisted");
    let metadata = tool_msg.metadata.as_ref().expect("metadata must be set");
    assert_eq!(
        metadata
            .get("truncated_in_loop")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(
        metadata
            .get("spill_path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| path.contains("sub-")),
        "persisted spill_path must reference the subagent dir: {metadata:?}"
    );
}

/// Drive `process_tool_results` with a single oversized tool result
/// and assert: the in-loop messages carry the truncated preview, the
/// session DB row has `truncated_in_loop: true` metadata, and the
/// spill file on disk holds the full original bytes.
#[tokio::test]
async fn process_tool_results_truncates_oversized_output() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime_with_truncate(dir.path().to_path_buf());

    let session_manager = SessionManager::new(SessionConfig::default());
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    // Synthesize a tool call + a 100 KB JSON-string result. 100 KB of
    // 'x' characters serialise to a string longer than the 4 KB
    // policy cap, so truncation will fire.
    let tool_call = ToolCall::new("call_huge", "fake_tool", "{}");
    let raw_text = "x".repeat(100 * 1024);
    let result_value = serde_json::Value::String(raw_text.clone());

    let mut messages: Vec<LlmMessage> = Vec::new();
    let mut records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut seq: u32 = 0;
    let invocation_ids = vec![uuid::Uuid::new_v4()];

    runtime.process_tool_results(
        std::slice::from_ref(&tool_call),
        vec![Ok(result_value)],
        &invocation_ids,
        &mut messages,
        &mut records,
        &mut seq,
        &session_manager,
        session.id,
        /* is_dm */ false,
    );

    // 1) The in-loop messages vec must carry the truncated preview,
    //    not the original.
    assert_eq!(messages.len(), 1);
    let preview = messages[0].content_str();
    assert!(
        preview.len() < raw_text.len(),
        "preview must be smaller than the original: preview={} raw={}",
        preview.len(),
        raw_text.len()
    );
    assert!(
        preview.contains("call_huge") || preview.contains("tool_call_huge"),
        "preview must reference the spill file by id"
    );
    assert!(preview.contains("Use `fs_grep`"));

    // 2) The spill file must exist and hold the full original bytes.
    let spill_path = dir.path().join("tool_call_huge.txt");
    assert!(spill_path.exists(), "spill file must be written");
    let spilled = std::fs::read_to_string(&spill_path).unwrap();
    assert_eq!(spilled, serde_json::Value::String(raw_text).to_string());

    // 3) The persisted session message must carry the truncation flag
    //    so `session_msg_to_llm` skips its own re-truncation pass.
    let history = session_manager.get_history(session.id).unwrap();
    let tool_msg = history
        .iter()
        .find(|m| matches!(m.role, alms_session::Role::Tool))
        .expect("tool result message must be persisted");
    let meta = tool_msg.metadata.as_ref().expect("metadata must be set");
    assert_eq!(
        meta.get("truncated_in_loop").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(meta.get("spill_path").and_then(|v| v.as_str()).is_some());
    assert!(
        meta.get("original_bytes")
            .and_then(|v| v.as_u64())
            .is_some()
    );
}

/// Symmetric counterpart: when the tool output is small, no
/// truncation, no spill file, no `truncated_in_loop` flag.
#[tokio::test]
async fn process_tool_results_passes_small_output_through() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime_with_truncate(dir.path().to_path_buf());

    let session_manager = SessionManager::new(SessionConfig::default());
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    let tool_call = ToolCall::new("call_small", "fake_tool", "{}");
    let result_value = serde_json::json!({"hello": "world"});

    let mut messages: Vec<LlmMessage> = Vec::new();
    let mut records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut seq: u32 = 0;
    let invocation_ids = vec![uuid::Uuid::new_v4()];

    runtime.process_tool_results(
        std::slice::from_ref(&tool_call),
        vec![Ok(result_value)],
        &invocation_ids,
        &mut messages,
        &mut records,
        &mut seq,
        &session_manager,
        session.id,
        false,
    );

    assert_eq!(messages.len(), 1);
    assert!(messages[0].content_str().contains("\"hello\":\"world\""));

    // No spill file should exist.
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .map(|it| it.flatten().collect())
        .unwrap_or_default();
    assert!(
        entries.is_empty(),
        "small output must not create a spill file"
    );

    // Metadata must NOT carry the truncated_in_loop flag.
    let history = session_manager.get_history(session.id).unwrap();
    let tool_msg = history
        .iter()
        .find(|m| matches!(m.role, alms_session::Role::Tool))
        .expect("tool result message must be persisted");
    let meta = tool_msg.metadata.as_ref().expect("metadata must be set");
    let flag = meta
        .get("truncated_in_loop")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(!flag, "small output must not be marked truncated_in_loop");
}

/// #851 acceptance test: simulate three sequential 50 KB tool calls
/// inside a single run loop and assert the cumulative size of the
/// LLM-visible messages stays bounded by the policy cap rather than
/// growing 50/100/150 KB across turns.
#[tokio::test]
async fn three_sequential_50kb_calls_stay_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime_with_truncate(dir.path().to_path_buf());

    let session_manager = SessionManager::new(SessionConfig::default());
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    let mut messages: Vec<LlmMessage> = Vec::new();
    let mut records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut seq: u32 = 0;

    for i in 0..3u32 {
        let tool_call = ToolCall::new(format!("call_{i}"), "fake_tool", "{}");
        let result_value = serde_json::Value::String("x".repeat(50 * 1024));
        let invocation_ids = vec![uuid::Uuid::new_v4()];

        runtime.process_tool_results(
            std::slice::from_ref(&tool_call),
            vec![Ok(result_value)],
            &invocation_ids,
            &mut messages,
            &mut records,
            &mut seq,
            &session_manager,
            session.id,
            false,
        );
    }

    // Each preview is well under the 4 KB cap + ~1 KB hint, so the
    // cumulative size of the in-loop messages must stay under
    // 3 * (4 KB + 2 KB) = 18 KB instead of the unbounded 3 * 50 KB
    // = 150 KB pre-#851 case.
    let cumulative: usize = messages.iter().map(|m| m.content_str().len()).sum();
    assert!(
        cumulative < 18 * 1024,
        "three 50 KB calls must collapse under the cap: got {cumulative} bytes"
    );

    // All three spills must exist on disk so the agent can recover
    // any of them via fs_read.
    for i in 0..3 {
        let spill = dir.path().join(format!("tool_call_{i}.txt"));
        assert!(
            spill.exists(),
            "spill {i} must be written: {}",
            spill.display()
        );
    }
}

/// `truncate_for_emit` must:
///   - return the original `Value` unchanged when truncation is a
///     no-op (small payload OR disabled policy)
///   - return a `Value::String(preview)` when truncation fires
///   - write the spill file as a side effect when truncation fires
///
/// Pre-#921 review fix #4, the audit log + `ToolEnd` SSE emitted the
/// raw untruncated bytes regardless of size. Post-fix, both surfaces
/// see the same preview the agent loop sees.
#[tokio::test]
async fn truncate_for_emit_passes_small_value_through() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime_with_truncate(dir.path().to_path_buf());

    let value = serde_json::json!({"hello": "world"});
    let out = runtime.truncate_for_emit("call_small", &value);
    assert_eq!(out, value, "small value must pass through unchanged");

    // No spill file should have been written.
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .map(|it| it.flatten().collect())
        .unwrap_or_default();
    assert!(
        entries.is_empty(),
        "small value must not create a spill file"
    );
}

#[tokio::test]
async fn truncate_for_emit_truncates_oversized_value() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = make_runtime_with_truncate(dir.path().to_path_buf());

    // Use a 100 KB string — above the 4 KB cap configured by
    // `make_runtime_with_truncate`.
    let value = serde_json::Value::String("y".repeat(100 * 1024));
    let out = runtime.truncate_for_emit("call_emit_big", &value);

    // The emitted value must be a JSON string (the preview), not the
    // original 100 KB structured value.
    let s = out.as_str().expect("truncated emit value must be a string");
    assert!(
        s.len() < 100 * 1024,
        "preview must be smaller than the original: {} vs {}",
        s.len(),
        100 * 1024
    );
    assert!(s.contains("call_emit_big"));
    assert!(s.contains("Use `fs_grep`"));

    // Spill file must exist.
    let spill = dir.path().join("tool_call_emit_big.txt");
    assert!(spill.exists(), "spill must be written by truncate_for_emit");
}

/// `truncate_for_emit` and `process_tool_results` are independent
/// callers of the same `truncate` service. When the service is
/// disabled, neither path should write a spill file.
#[tokio::test]
async fn truncate_for_emit_is_no_op_when_policy_disabled() {
    // Construct a runtime with an explicitly disabled policy.
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = make_runtime_with_truncate(dir.path().to_path_buf());
    runtime.tool_output_truncate_policy =
        crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled();

    let value = serde_json::Value::String("z".repeat(100 * 1024));
    let out = runtime.truncate_for_emit("call_disabled", &value);
    assert_eq!(
        out, value,
        "disabled policy must pass even an oversized value through unchanged"
    );

    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .map(|it| it.flatten().collect())
        .unwrap_or_default();
    assert!(
        entries.is_empty(),
        "disabled policy must not create a spill file"
    );
}
