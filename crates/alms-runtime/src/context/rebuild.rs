//! Persisted-message → `LlmMessage` reconstruction.
//!
//! Bridges the session-store representation (`Message` with `Role` and
//! `Content` enum) to the provider-neutral `LlmMessage` shape consumed by
//! the LLM client. Reconstructs structured tool call / tool result messages
//! so the LLM has full visibility of previous tool executions across runs.
//!
//! Also handles the #921 spill-path repair: when a tool-result message
//! references a relative spill file that has been swept by the >7 day
//! retention sweep, [`session_msg_to_llm`] rewrites the trailing recovery
//! hint to indicate the file is no longer available so the LLM doesn't try
//! to `fs_read` an ENOENT path.
//!
//! Pure free functions — no `&self`, no shared state. The `workspace_root`
//! used to resolve relative spill paths is threaded in as an explicit
//! argument so this module is independently testable.

use crate::llm_types::{LlmMessage, ToolCall};
use alms_core::truncate_to_char_boundary;
use alms_session::{Content, Message, Role};
use std::path::Path;

use super::error_markers::is_error_marker;

/// Convert a session [`Message`] to an [`LlmMessage`].
///
/// Reconstructs structured tool call / tool result messages from persisted
/// format so the LLM has full visibility of previous tool executions across
/// runs.
///
/// `workspace_root` is used to resolve the relative `spill_path` metadata
/// when rebuilding tool-result messages from session history. When the
/// referenced spill file no longer exists on disk (the per-run sweep has
/// expired it), the trailing recovery hint is swapped for an "expired"
/// notice so an agent reading an older session doesn't get told to
/// `fs_read` a path that returns ENOENT (#921 review fix #3). Pass `None`
/// when no workspace root is available — the existence check is then
/// skipped and the original hint is preserved (graceful degradation: the
/// agent may try `fs_read` and fail, but the LLM is not given an
/// inaccurate "expired" notice).
pub(super) fn session_msg_to_llm(msg: &Message, workspace_root: Option<&Path>) -> LlmMessage {
    // Error markers (#874) are converted to a `user` message with a
    // `[Error]` prefix BEFORE the per-role match below, so they
    // survive `strip_mid_history_system_markers` and reach the LLM.
    // Without this, the existing canonical-shape pass would drop
    // them and the agent would never see the prior failure.
    if is_error_marker(msg) {
        return LlmMessage::user(format!("[Error] {}", msg.content.to_display_string()));
    }

    match (&msg.role, &msg.content) {
        // Reconstruct structured assistant message with tool_calls
        (Role::Assistant, Content::ToolCall { name, params }) => {
            let tool_call_id = msg
                .metadata
                .as_ref()
                .and_then(|m| m.get("tool_call_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Heal legacy `params: ""` poison written by the
            // pre-#967 persistence path: a no-args Anthropic tool
            // call (where `arguments == ""`) was stored as
            // `Value::String("")` instead of `Value::Object({})`,
            // and re-serializing that on the wire produced
            // `tool_use.input: ""` which Anthropic 400s. The
            // adapters' `normalize_tool_args` would also catch this
            // (wrapping under `_raw`), but normalizing here means
            // the rebuilt arguments are the truthful `{}` rather
            // than `{"_raw":"\"\""}`.
            let params_str = if matches!(params, serde_json::Value::String(s) if s.is_empty()) {
                "{}".to_string()
            } else {
                params.to_string()
            };
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new(tool_call_id, name.clone(), params_str)]),
                tool_call_id: None,
            }
        }
        // Reconstruct tool result with correct tool_call_id
        (Role::Tool, Content::ToolResult { tool_id, result }) => {
            let result_str = result.to_string();
            // When the in-loop truncation service (#851) already
            // capped this message, the persisted bytes are exactly
            // what the live agent saw. Skip the legacy 2000-byte
            // re-truncation in that case — re-truncating would shrink
            // the head+tail preview to ~2 KB and discard the spill-
            // path hint the agent needs to recover the full bytes.
            let truncated_in_loop = msg
                .metadata
                .as_ref()
                .and_then(|m| m.get("truncated_in_loop"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let content = if truncated_in_loop {
                // Detect a swept spill file (#921 review fix #3): if the
                // recorded `spill_path` no longer exists on disk (>7 day
                // retention sweep has expired it), rewrite the trailing
                // recovery hint so the agent doesn't try to `fs_read` a
                // path that returns ENOENT.
                let spill_path = msg
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("spill_path"))
                    .and_then(|v| v.as_str());
                if let Some(rel) = spill_path
                    && let Some(root) = workspace_root
                    && spill_file_missing(root, rel)
                {
                    rewrite_hint_as_expired(&result_str, rel)
                } else {
                    result_str
                }
            } else if result_str.len() > 2000 {
                // Legacy fallback for tool-result messages persisted
                // before #851 (or when the in-loop service is
                // disabled by config). Keeps the pre-#851 wire shape
                // byte-identical so existing fixtures continue to
                // pass.
                format!(
                    "{}... [truncated, {} bytes total]",
                    truncate_to_char_boundary(&result_str, 2000),
                    result_str.len()
                )
            } else {
                result_str
            };
            LlmMessage::tool_result(tool_id.clone(), content)
        }
        (Role::System, _) => LlmMessage::system(msg.content.to_display_string()),
        (Role::User, _) => LlmMessage::user(msg.content.to_display_string()),
        (Role::Assistant, _) => LlmMessage::assistant(msg.content.to_display_string()),
        (Role::Tool, _) => LlmMessage::tool_result(msg.id.clone(), msg.content.to_display_string()),
    }
}

/// Resolve the relative `spill_path` from a tool-result message's
/// metadata against `workspace_root` and return `true` when the file
/// does NOT exist on disk (i.e. the retention sweep has expired it).
///
/// `rel` is the path emitted by `tool_output_truncate::truncate` —
/// either workspace-relative (when a workspace root was passed in at
/// truncate time) or an absolute path (no workspace root). We try
/// `workspace_root.join(rel)` first; when that doesn't exist *and* the
/// raw `rel` parses as an absolute path, we try that too. Either path
/// existing is enough to consider the spill "live"; both missing means
/// it has been swept (or never made it to disk).
fn spill_file_missing(workspace_root: &Path, rel: &str) -> bool {
    let joined = workspace_root.join(rel);
    if joined.exists() {
        return false;
    }
    let raw = Path::new(rel);
    if raw.is_absolute() && raw.exists() {
        return false;
    }
    true
}

/// Rewrite the trailing recovery hint in a head+tail-truncated tool
/// result to indicate the spill file is no longer available.
///
/// `original` is the persisted preview that ends with the
/// `[The tool output was truncated to N KB. Full output saved to:
///  \`<rel>\`...]` block produced by
/// `tool_output_truncate::build_preview`. We trim that trailing block
/// (everything from the last `[The tool output was truncated to`
/// occurrence onward) and append a short "expired" notice. When the
/// marker is absent for any reason we just append the notice — the
/// degradation is safe even if the original shape is unexpected.
fn rewrite_hint_as_expired(original: &str, rel: &str) -> String {
    const MARKER: &str = "[The tool output was truncated to";
    let trimmed = match original.rfind(MARKER) {
        Some(idx) => original[..idx].trim_end_matches('\n').to_string(),
        None => original.trim_end_matches('\n').to_string(),
    };
    format!(
        "{trimmed}\n\n[The tool output was truncated. The full-output spill file \
         (`{rel}`) is no longer available — retention period has expired. Only \
         this preview survives.]\n"
    )
}

#[cfg(test)]
mod tests {
    use super::super::ContextBuilder;
    use super::super::tests::{default_builder, make_msg};
    use alms_core::{Timestamp, config::ContextConfig};
    use alms_session::{Content, Message, Role};

    #[test]
    fn test_tool_result_truncation() {
        let long_result = "x".repeat(5000);
        let content = Content::ToolResult {
            tool_id: "test".into(),
            result: serde_json::Value::String(long_result),
        };
        let result = content.to_display_string();
        assert!(result.contains("[truncated"));
        assert!(result.len() < 3000);
    }

    #[test]
    fn test_tool_call_reconstructed_in_context() {
        let config = ContextConfig {
            strategy: "full".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "run ls"),
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "shell_exec".to_string(),
                    params: serde_json::json!({"command": "ls"}),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_123"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "call_123".to_string(),
                    result: serde_json::json!("file1.txt\nfile2.txt"),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            make_msg(Role::Assistant, "Here are the files."),
        ];

        let messages = builder.build("System", &history, "now what?", None);

        // system + 4 history + current = 6
        assert_eq!(messages.len(), 6);

        // Tool call message: assistant with tool_calls array
        let tc_msg = &messages[2];
        assert_eq!(tc_msg.role, "assistant");
        assert!(tc_msg.content.is_none());
        let calls = tc_msg.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_123");
        assert_eq!(calls[0].function.name, "shell_exec");

        // Tool result message: role=tool with tool_call_id
        let tr_msg = &messages[3];
        assert_eq!(tr_msg.role, "tool");
        assert_eq!(tr_msg.tool_call_id.as_deref(), Some("call_123"));
        assert!(tr_msg.content_str().contains("file1.txt"));
    }

    /// Regression test for #967 — legacy sessions persisted by the
    /// pre-fix code path stored a no-args Anthropic tool call as
    /// `Content::ToolCall { params: Value::String("") }`. Re-serializing
    /// that on the wire produced `tool_use.input: ""` which Anthropic
    /// rejects (`"Input should be an object"`), wedging the
    /// conversation across runs. The rebuild path now heals the poison
    /// so already-persisted sessions can recover without manual DB
    /// surgery.
    #[test]
    fn test_legacy_empty_string_params_heals_to_empty_object() {
        let config = ContextConfig {
            strategy: "full".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "list files"),
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                // The exact poison shape from #967.
                content: Content::ToolCall {
                    name: "fs_list".to_string(),
                    params: serde_json::Value::String(String::new()),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_poisoned"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "call_poisoned".to_string(),
                    result: serde_json::json!("[]"),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            make_msg(Role::Assistant, "ok done"),
        ];

        let messages = builder.build("System", &history, "and now?", None);

        let tc_msg = messages
            .iter()
            .find(|m| m.role == "assistant" && m.tool_calls.is_some())
            .expect("rebuilt assistant tool_call message must exist");
        let calls = tc_msg.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].function.arguments, "{}",
            "legacy `params: \"\"` poison must heal to `arguments: \"{{}}\"` so the \
             Anthropic adapter serializes `tool_use.input: {{}}` and not `\"\"`",
        );

        // And confirm the parse round-trip: reconstructed `arguments`
        // must parse back to a JSON object so subsequent normalize
        // passes are no-ops.
        let parsed: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn test_parallel_tool_calls_grouped() {
        let config = ContextConfig {
            strategy: "full".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Simulate 3 parallel tool calls persisted as separate messages,
        // followed by 3 tool results — the typical pattern for join_all.
        let history = vec![
            make_msg(Role::User, "list files and check disk"),
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "shell_exec".to_string(),
                    params: serde_json::json!({"command": "ls"}),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_A"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "shell_exec".to_string(),
                    params: serde_json::json!({"command": "df -h"}),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_B"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "echo".to_string(),
                    params: serde_json::json!({"text": "done"}),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_C"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "call_A".to_string(),
                    result: serde_json::json!("file1.txt"),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "call_B".to_string(),
                    result: serde_json::json!("/dev/sda1 50G"),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "call_C".to_string(),
                    result: serde_json::json!("done"),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            make_msg(Role::Assistant, "Here's what I found."),
        ];

        let messages = builder.build("System", &history, "next?", None);

        // system + 1 user + 1 grouped assistant + 3 tool results + 1 assistant text + current = 8
        // (3 tool calls collapsed into 1)
        assert_eq!(messages.len(), 8);

        // The grouped assistant message should have 3 tool_calls
        let tc_msg = &messages[2];
        assert_eq!(tc_msg.role, "assistant");
        assert!(tc_msg.content.is_none());
        let calls = tc_msg.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].id, "call_A");
        assert_eq!(calls[1].id, "call_B");
        assert_eq!(calls[2].id, "call_C");
        assert_eq!(calls[0].function.name, "shell_exec");
        assert_eq!(calls[2].function.name, "echo");

        // Tool results should follow individually
        assert_eq!(messages[3].role, "tool");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("call_A"));
        assert_eq!(messages[4].role, "tool");
        assert_eq!(messages[4].tool_call_id.as_deref(), Some("call_B"));
        assert_eq!(messages[5].role, "tool");
        assert_eq!(messages[5].tool_call_id.as_deref(), Some("call_C"));

        // Final assistant text
        assert_eq!(messages[6].role, "assistant");
        assert_eq!(messages[6].content_str(), "Here's what I found.");
    }

    // -- #851: in-loop truncation interaction with session_msg_to_llm ---------

    /// When a tool result message carries `truncated_in_loop: true` in its
    /// metadata, `session_msg_to_llm` must NOT apply its legacy 2000-byte
    /// re-truncation — the persisted bytes ARE the bytes the live agent
    /// saw, including the spill-path hint, and re-truncating would shred
    /// the recovery instructions.
    #[test]
    fn session_msg_to_llm_skips_re_truncation_when_in_loop_flag_set() {
        let builder = default_builder();
        let preview = format!("preview head\n{}\npreview tail", "x".repeat(3000));
        // Sanity: the preview is well above 2000 bytes so the legacy path
        // would trigger.
        assert!(preview.len() > 2000);

        let msg = Message {
            id: "m1".to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "call_abc".to_string(),
                result: serde_json::Value::String(preview.clone()),
            },
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "ok": true,
                "tool_invocation_id": "abc",
                "truncated_in_loop": true,
                "spill_path": ".alms/tool-output/run1/tool_call_abc.txt",
                "original_bytes": 100_000,
                "original_lines": 1,
            })),
        };

        let llm = builder.session_msg_to_llm(&msg);
        // The serialised tool-result string must contain the full preview
        // body — no `... [truncated, N bytes total]` suffix.
        assert!(
            !llm.content_str().contains("[truncated,"),
            "in-loop-truncated message must not be re-truncated by session_msg_to_llm"
        );
        assert!(
            llm.content_str().len() > 2000,
            "preview must survive past the legacy 2000-byte cap"
        );
    }

    /// Symmetric counterpart: when the in-loop flag is absent, the legacy
    /// 2000-byte re-truncation still fires. This pins the
    /// backward-compatibility guarantee for tool result messages persisted
    /// before #851 (or by deployments where the in-loop service is
    /// disabled in `alms.toml`).
    #[test]
    fn session_msg_to_llm_still_truncates_when_flag_absent() {
        let builder = default_builder();
        let big = "x".repeat(5000);

        let msg = Message {
            id: "m1".to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "call_abc".to_string(),
                result: serde_json::Value::String(big.clone()),
            },
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "ok": true,
                "tool_invocation_id": "abc",
            })),
        };

        let llm = builder.session_msg_to_llm(&msg);
        let body = llm.content_str();
        assert!(
            body.contains("[truncated,"),
            "legacy path must still truncate when truncated_in_loop is absent"
        );
        // 2000-byte preview + suffix string -> stays under, say, 2500
        // bytes for the entire message.
        assert!(
            body.len() < 2500,
            "legacy path caps at 2000 bytes plus suffix"
        );
    }

    // -- #921 review fix #3: stale spill_path detection -----------------------

    /// When a tool-result message references a `spill_path` that has been
    /// swept (>7d retention), `session_msg_to_llm` must rewrite the
    /// recovery hint to indicate the file is no longer available so the
    /// LLM doesn't try to `fs_read` an ENOENT path.
    #[test]
    fn session_msg_to_llm_rewrites_hint_when_spill_file_missing() {
        let workspace_dir = tempfile::tempdir().unwrap();
        let builder =
            default_builder().with_workspace_root(Some(workspace_dir.path().to_path_buf()));

        // Persisted preview with the full hint text from `build_preview`.
        let preview = format!(
            "head data\n\n... [50000 bytes / 1 lines omitted] ...\n\ntail data\n\n[The tool output \
             was truncated to 32 KB. Full output saved to: `{rel}` (50000 bytes, 1 lines). Use \
             `fs_grep` to search the full content or `fs_read` with `offset`/`limit` to view \
             specific sections.]\n",
            rel = "tool-output/run1/tool_call_abc.txt"
        );

        let msg = Message {
            id: "m1".to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "call_abc".to_string(),
                result: serde_json::Value::String(preview.clone()),
            },
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "ok": true,
                "tool_invocation_id": "abc",
                "truncated_in_loop": true,
                // This relative path does NOT exist under workspace_dir.
                "spill_path": "tool-output/run1/tool_call_abc.txt",
                "original_bytes": 50_000,
                "original_lines": 1,
            })),
        };

        let llm = builder.session_msg_to_llm(&msg);
        let body = llm.content_str();
        // The original "Use `fs_grep`..." hint must be gone.
        assert!(
            !body.contains("Use `fs_grep`"),
            "stale fs_grep hint must be removed when spill is missing"
        );
        // The replacement notice must mention "retention" and "expired".
        assert!(
            body.contains("retention period has expired"),
            "expired hint must be present: {body}"
        );
        // The head/tail body itself must survive the rewrite.
        assert!(body.contains("head data"));
        assert!(body.contains("tail data"));
    }

    /// Symmetric counterpart: when the spill file IS still present on
    /// disk, the recovery hint must remain intact so the agent can
    /// `fs_read` it normally.
    #[test]
    fn session_msg_to_llm_keeps_hint_when_spill_file_present() {
        let workspace_dir = tempfile::tempdir().unwrap();
        // Materialise the spill file under workspace_dir so the existence
        // check passes.
        let spill_dir = workspace_dir.path().join("tool-output").join("run1");
        std::fs::create_dir_all(&spill_dir).unwrap();
        let spill_file = spill_dir.join("tool_call_abc.txt");
        std::fs::write(&spill_file, b"original payload").unwrap();

        let builder =
            default_builder().with_workspace_root(Some(workspace_dir.path().to_path_buf()));

        let preview = format!(
            "head data\n\n... omitted ...\n\ntail data\n\n[The tool output was truncated to 32 KB. \
             Full output saved to: `{rel}` (50000 bytes, 1 lines). Use `fs_grep` to search the full \
             content or `fs_read` with `offset`/`limit` to view specific sections.]\n",
            rel = "tool-output/run1/tool_call_abc.txt"
        );

        let msg = Message {
            id: "m1".to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "call_abc".to_string(),
                result: serde_json::Value::String(preview.clone()),
            },
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "ok": true,
                "tool_invocation_id": "abc",
                "truncated_in_loop": true,
                "spill_path": "tool-output/run1/tool_call_abc.txt",
                "original_bytes": 50_000,
                "original_lines": 1,
            })),
        };

        let llm = builder.session_msg_to_llm(&msg);
        let body = llm.content_str();
        // The recovery hint must be intact.
        assert!(
            body.contains("Use `fs_grep`"),
            "fs_grep hint must survive when spill file exists"
        );
        // The expired notice must NOT appear.
        assert!(!body.contains("retention period has expired"));
    }

    /// Without a workspace root configured the builder cannot resolve
    /// relative spill paths, so it MUST leave the hint unchanged (graceful
    /// degradation: the agent may try `fs_read` and fail, but the LLM is
    /// not given an inaccurate "expired" notice).
    #[test]
    fn session_msg_to_llm_leaves_hint_alone_without_workspace_root() {
        let builder = default_builder(); // no workspace_root

        let preview =
            "head\n\n... omitted ...\n\ntail\n\n[The tool output was truncated to 32 KB. \
             Full output saved to: `tool-output/run1/tool_call_abc.txt` (50000 bytes, 1 lines). \
             Use `fs_grep` to search the full content or `fs_read` with `offset`/`limit` to view \
             specific sections.]\n"
                .to_string();

        let msg = Message {
            id: "m1".to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "call_abc".to_string(),
                result: serde_json::Value::String(preview.clone()),
            },
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "ok": true,
                "tool_invocation_id": "abc",
                "truncated_in_loop": true,
                "spill_path": "tool-output/run1/tool_call_abc.txt",
                "original_bytes": 50_000,
                "original_lines": 1,
            })),
        };

        let llm = builder.session_msg_to_llm(&msg);
        // Without a workspace root the existence check is skipped, so the
        // hint must survive intact — including the spill path reference
        // and the `Use \`fs_grep\`` recovery instruction. (The session
        // round-trip JSON-encodes the inner string, so we only check that
        // the original recovery hint substring is present, not byte
        // equality with the bare preview.)
        let body = llm.content_str();
        assert!(
            body.contains("Use `fs_grep`"),
            "fs_grep hint must survive when workspace_root is unset: {body}"
        );
        assert!(
            !body.contains("retention period has expired"),
            "expired notice must NOT appear when workspace_root is unset"
        );
    }
}
