//! Error-marker classification and legacy run-boundary deduplication.
//!
//! The `persist_error_marker` path in `gateway::runs::markers` (issue #874)
//! writes synthetic `Role::System` messages tagged with `metadata.kind ==
//! "error"` so mid-run failures (LLM 4xx/5xx, run cancellation, runtime
//! construction error) survive into the agent's next turn. The context
//! builder rewrites those markers into `[Error] ...` user messages via
//! `rebuild::session_msg_to_llm` so they reach the LLM (otherwise
//! `normalize::strip_mid_history_system_markers` would drop them on the
//! floor).
//!
//! Pre-#912 the lifecycle layer ALSO wrote a `(run failed) ...` /
//! `(run cancelled)` marker for every failed run, which sat right next to
//! the runtime layer's `[Run failed: ...]` / `[Run cancelled by user]` text
//! bubble — both reached the LLM and the agent saw the same failure twice.
//! #912 stops new failed runs from writing the lifecycle-layer marker; this
//! module's [`has_legacy_duplicate_run_boundary_markers`] /
//! [`filter_legacy_duplicate_run_boundary_markers`] pair handles the
//! reconstruction-side fix for legacy SQLite DBs that captured a failed run
//! before #912 (Tim's F2 finding on PR #930).
//!
//! Pure free functions — no `&self`, no shared state.

use alms_session::{Content, Message, Role};

/// Returns `true` when the message is an error marker persisted by
/// `persist_error_marker` in the gateway (issue #874).
///
/// Error markers are `Role::System` synthetic markers tagged with
/// `metadata.kind == "error"`. They surface mid-run failures (LLM
/// 4xx/5xx, run cancellation, runtime construction error) into the
/// agent's context so a follow-up turn like "why did that fail?"
/// gives the LLM the error text without re-quoting.
pub(super) fn is_error_marker(msg: &Message) -> bool {
    if msg.role != Role::System {
        return false;
    }
    msg.metadata
        .as_ref()
        .and_then(|m| m.get("kind"))
        .and_then(|v| v.as_str())
        == Some("error")
}

/// Returns `true` when the message is the lifecycle-layer
/// `(run failed) ...` / `(run cancelled)` `kind: "error"` marker
/// removed in #912.
///
/// Identified by `metadata.kind == "error"` AND `metadata.type ==
/// "run_boundary"` — exactly the shape persisted by the four
/// `persist_error_marker` call sites in `gateway::runs::lifecycle`'s
/// `Cancelled`, `CancelledWithToolCalls`, `FailedWithToolCalls`, and
/// generic `Err(_)` arms before #912.  Crucially does NOT match
/// `type: "runtime_init_error"` — that marker has no runtime-layer
/// counterpart and is the only error record for runtime-construction
/// failures, so it must be kept.
pub(super) fn is_legacy_lifecycle_error_marker(msg: &Message) -> bool {
    if !is_error_marker(msg) {
        return false;
    }
    msg.metadata
        .as_ref()
        .and_then(|m| m.get("type"))
        .and_then(|v| v.as_str())
        == Some("run_boundary")
}

/// Returns `true` when the message is the runtime-layer
/// `[Run failed: ...]` or `[Run cancelled by user]` text bubble
/// persisted by `AgentRuntime::finish_run` on the `Err(_)` and
/// `Err(Cancelled)` arms.
///
/// Matches both the regular-session shape (`Role::Assistant` text)
/// and the DM-session shape (`Role::User` with `from_agent`
/// metadata) — both bubbles share the literal `[Run failed:` /
/// `[Run cancelled by user]` content prefix.
pub(super) fn is_runtime_layer_failure_bubble(msg: &Message) -> bool {
    // Runtime-layer bubbles are never persisted as `Role::System`,
    // so a `System` here is necessarily a marker, not a bubble.
    if msg.role == Role::System {
        return false;
    }
    match &msg.content {
        Content::Text(t) => {
            t.starts_with("[Run failed:") || t.starts_with("[Run cancelled by user]")
        }
        _ => false,
    }
}

/// Returns `true` when `history` contains a legacy lifecycle-layer
/// `kind: "error"` `type: "run_boundary"` marker immediately after
/// (or before) a runtime-layer failure bubble — the duplicate
/// pattern from pre-#912 SQLite DBs.  Used as a cheap pre-check to
/// avoid building a fresh `Vec<Message>` when the common case
/// (post-#912 data, no duplicates) holds.
pub(super) fn has_legacy_duplicate_run_boundary_markers(history: &[Message]) -> bool {
    for (idx, msg) in history.iter().enumerate() {
        if !is_legacy_lifecycle_error_marker(msg) {
            continue;
        }
        // Pre-#912 the runtime-layer bubble is written first, then
        // the lifecycle-layer marker — so the bubble is at idx-1.
        // We also check idx+1 defensively in case a future ordering
        // change inverts that.
        let prev_is_bubble = idx
            .checked_sub(1)
            .and_then(|i| history.get(i))
            .is_some_and(is_runtime_layer_failure_bubble);
        let next_is_bubble = history
            .get(idx + 1)
            .is_some_and(is_runtime_layer_failure_bubble);
        if prev_is_bubble || next_is_bubble {
            return true;
        }
    }
    false
}

/// Drop legacy lifecycle-layer `kind: "error"` `type: "run_boundary"`
/// markers that sit adjacent to a runtime-layer failure bubble,
/// returning a fresh `Vec<Message>` with the duplicates removed.
///
/// Caller is responsible for the cheap pre-check
/// ([`has_legacy_duplicate_run_boundary_markers`]) — calling this on
/// post-#912 data simply clones the slice unchanged, but allocating
/// a fresh `Vec` per build call when there's nothing to filter is
/// wasteful.
pub(super) fn filter_legacy_duplicate_run_boundary_markers(history: &[Message]) -> Vec<Message> {
    history
        .iter()
        .enumerate()
        .filter(|(idx, msg)| {
            if !is_legacy_lifecycle_error_marker(msg) {
                return true;
            }
            let prev_is_bubble = idx
                .checked_sub(1)
                .and_then(|i| history.get(i))
                .is_some_and(is_runtime_layer_failure_bubble);
            let next_is_bubble = history
                .get(idx + 1)
                .is_some_and(is_runtime_layer_failure_bubble);
            // Drop only when adjacent to a bubble.  Markers that
            // appear without an adjacent bubble (corrupted DB,
            // unusual write order, future code path) are kept so
            // the agent still sees the failure on follow-up.
            !(prev_is_bubble || next_is_bubble)
        })
        .map(|(_, msg)| msg.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::ContextBuilder;
    use super::super::tests::make_msg;
    use alms_core::{Timestamp, config::ContextConfig};
    use alms_session::{Content, Message, Role};

    /// Helper: build an error-marker message matching the shape persisted
    /// by `gateway::runs::markers::persist_error_marker` (#874).
    fn make_error_marker(text: &str, status: &str, error: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text(text.to_string()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "synthetic": true,
                "kind": "error",
                "type": "run_boundary",
                "status": status,
                "error": error,
            })),
        }
    }

    /// Run-failed error markers (#874) must reach the LLM context as a
    /// `user` message so a follow-up turn ("why did that fail?") gives the
    /// agent the error text without re-quoting. Without the
    /// `is_error_marker` rewrite in `session_msg_to_llm`, the standard
    /// `strip_mid_history_system_markers` pass would drop the marker and
    /// the agent would never see the prior failure.
    #[test]
    fn error_marker_survives_as_user_message() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "List files in /tmp"),
            make_error_marker(
                "(run failed) Anthropic 500: server error",
                "failed",
                "Anthropic 500: server error",
            ),
        ];

        let messages = builder.build(
            "You are a helpful agent.",
            &history,
            "why did that fail?",
            None,
        );

        // No mid-history system messages survive normalization.
        assert!(
            messages[1..].iter().all(|m| m.role != "system"),
            "no system message may appear after the system prefix"
        );

        // The error marker must reach the LLM as a user message tagged
        // with the [Error] prefix so the agent can answer the follow-up.
        let error_visible = messages.iter().any(|m| {
            m.role == "user"
                && m.content.as_deref().is_some_and(|s| {
                    s.contains("[Error]") && s.contains("Anthropic 500: server error")
                })
        });
        assert!(
            error_visible,
            "error marker must be rewritten to a `user` [Error] message so the LLM sees it; got: {:?}",
            messages
                .iter()
                .map(|m| (m.role.clone(), m.content.clone().unwrap_or_default()))
                .collect::<Vec<_>>()
        );

        // The trailing user-input invariant must hold: the user's actual
        // follow-up question is the last message.
        let last = messages.last().expect("non-empty");
        assert_eq!(last.role, "user");
        assert!(
            last.content
                .as_deref()
                .is_some_and(|s| s.contains("why did that fail?")),
            "trailing user message must be the fresh input, not the rewritten error marker"
        );
    }

    /// Cancellation markers (#874) follow the same rewrite path as
    /// run-failed markers — the `kind: "error"` flag is what matters,
    /// not the specific `status`/`error_kind` value.
    #[test]
    fn cancelled_marker_survives_as_user_message() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "Run a long-running task"),
            make_error_marker("(run cancelled)", "cancelled", "user cancelled"),
        ];

        let messages = builder.build("System.", &history, "try again", None);

        let cancelled_visible = messages.iter().any(|m| {
            m.role == "user"
                && m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("[Error]") && s.contains("(run cancelled)"))
        });
        assert!(
            cancelled_visible,
            "cancellation marker must be rewritten to [Error] user message"
        );
    }

    /// Non-error system markers (e.g. job notifications, completed
    /// run_boundary) must continue to be stripped — only `kind: "error"`
    /// markers get the rewrite. This protects the existing canonical
    /// invariant (no mid-history system messages reach the LLM).
    #[test]
    fn non_error_system_markers_still_stripped() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // A completed run_boundary marker (no `kind: "error"`) should be
        // stripped — only failed/cancelled markers carry `kind: "error"`.
        let completed_marker = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text("(run completed)".to_string()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "synthetic": true,
                "type": "run_boundary",
                "status": "completed",
            })),
        };

        let history = vec![
            make_msg(Role::User, "hi"),
            make_msg(Role::Assistant, "hello"),
            completed_marker,
        ];

        let messages = builder.build("System.", &history, "next?", None);

        // The completed marker must NOT appear in the context.
        assert!(
            !messages.iter().any(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("(run completed)"))
            }),
            "non-error system markers must continue to be stripped"
        );
    }

    // -- #912 follow-up: legacy duplicate run_boundary marker filter ---------

    /// Regression test for PR #930 follow-up F2 — Tim's "forward-only dedup"
    /// finding.  Pre-#912 SQLite DBs persist BOTH the runtime-layer
    /// `[Run failed: ...]` assistant bubble AND the lifecycle-layer
    /// `(run failed) ...` `kind: "error"` `type: "run_boundary"` system
    /// marker for every failed run.  #912 stops new failed runs from writing
    /// the marker, but existing DBs still surface both records during
    /// context reload.  This test pins the reconstruction-side filter that
    /// drops the duplicate marker so legacy DBs match new-DB display
    /// behaviour without a data migration.
    #[test]
    fn legacy_duplicate_run_boundary_marker_dropped_when_adjacent_to_runtime_bubble() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Mimic the exact pre-#912 SQLite shape: runtime-layer bubble at
        // index 1 (Role::Assistant), then lifecycle-layer marker at index
        // 2 (Role::System with kind: "error", type: "run_boundary",
        // status: "failed").  Both records carry the same conceptual
        // failure event.
        let history = vec![
            make_msg(Role::User, "do something"),
            make_msg(Role::Assistant, "[Run failed: Provider error]"),
            make_error_marker("(run failed) Provider error", "failed", "Provider error"),
        ];

        let messages = builder.build(
            "You are a helpful agent.",
            &history,
            "why did that fail?",
            None,
        );

        // The runtime-layer assistant bubble must survive as the
        // canonical record (Atlas + Alper's decision on #912).
        let bubble_count = messages
            .iter()
            .filter(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("[Run failed: Provider error]"))
            })
            .count();
        assert_eq!(
            bubble_count, 1,
            "the runtime-layer `[Run failed: ...]` bubble must survive as the canonical record; got {bubble_count} occurrences in {messages:?}"
        );

        // The lifecycle-layer marker must NOT surface as a `[Error]
        // (run failed) ...` user message — pre-fix it would, because
        // `session_msg_to_llm` rewrites `kind: "error"` markers into
        // `[Error] ...` user messages BEFORE
        // `strip_mid_history_system_markers` runs.  Post-fix the
        // marker is dropped during context build, so the rewrite never
        // fires and the LLM sees only the bubble.
        let legacy_marker_visible = messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|s| s.contains("[Error]") && s.contains("(run failed)"))
        });
        assert!(
            !legacy_marker_visible,
            "legacy lifecycle-layer marker must not surface to the LLM when an adjacent runtime-layer bubble carries the same event; messages: {messages:?}"
        );
    }

    /// Same legacy-duplicate test, but for the cancellation pair —
    /// `[Run cancelled by user]` runtime bubble + `(run cancelled)`
    /// `kind: "error"` `status: "cancelled"` lifecycle marker.  Mirrors
    /// the pre-#912 shape for the `Cancelled` and
    /// `CancelledWithToolCalls` arms.
    #[test]
    fn legacy_duplicate_cancelled_marker_dropped_when_adjacent_to_runtime_bubble() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "long-running task"),
            make_msg(Role::Assistant, "[Run cancelled by user]"),
            make_error_marker("(run cancelled)", "cancelled", "user cancelled"),
        ];

        let messages = builder.build("System.", &history, "try again", None);

        let bubble_count = messages
            .iter()
            .filter(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("[Run cancelled by user]"))
            })
            .count();
        assert_eq!(
            bubble_count, 1,
            "the runtime-layer `[Run cancelled by user]` bubble must survive; got {bubble_count} in {messages:?}"
        );

        let legacy_marker_visible = messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|s| s.contains("[Error]") && s.contains("(run cancelled)"))
        });
        assert!(
            !legacy_marker_visible,
            "legacy cancellation marker must not surface to the LLM when an adjacent bubble exists; messages: {messages:?}"
        );
    }

    /// New (post-#912) DBs only have the runtime-layer bubble — no
    /// lifecycle-layer marker is written for the four removed call
    /// sites.  The filter must be a no-op on this shape: the bubble
    /// reaches the LLM as a regular conversation turn, and the agent
    /// can answer "why did that fail?" using its content directly (no
    /// `[Error]` rewrite needed because there is no marker to rewrite).
    #[test]
    fn post_912_history_with_only_runtime_bubble_unchanged_by_filter() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "do something"),
            make_msg(Role::Assistant, "[Run failed: Provider error]"),
        ];

        let messages = builder.build("System.", &history, "why?", None);

        let bubble_count = messages
            .iter()
            .filter(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("[Run failed: Provider error]"))
            })
            .count();
        assert_eq!(
            bubble_count, 1,
            "post-#912 history must keep the single runtime-layer bubble untouched"
        );
    }

    /// `runtime_init_error` markers (`type: "runtime_init_error"`)
    /// fire when `AgentRuntime::new()` itself fails — strictly before
    /// `finish_run` could possibly run, so they have NO runtime-layer
    /// counterpart.  The legacy-duplicate filter must NOT match these
    /// markers (matching them would drop the only error record for
    /// runtime-construction failures).  This is the negative-space
    /// guard for `is_legacy_lifecycle_error_marker`.
    #[test]
    fn runtime_init_error_marker_not_matched_by_legacy_dedup() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Mimic the shape persisted by lifecycle.rs's
        // runtime_init_error path (kept by #912).  No runtime-layer
        // bubble is present because the runtime never started.
        let init_error_marker = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text("(runtime initialization failed) Provider error".to_string()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "synthetic": true,
                "kind": "error",
                "type": "runtime_init_error",
                "status": "failed",
                "error": "Provider error",
                "error_kind": "runtime_init",
            })),
        };

        let history = vec![make_msg(Role::User, "first turn"), init_error_marker];

        let messages = builder.build("System.", &history, "follow-up", None);

        // The init-error marker must surface to the LLM as a `[Error]
        // ...` user message (existing #874 behaviour) — the legacy
        // dedup filter must NOT have touched it because its `type` is
        // `runtime_init_error`, not `run_boundary`.
        let init_error_visible = messages.iter().any(|m| {
            m.role == "user"
                && m.content.as_deref().is_some_and(|s| {
                    s.contains("[Error]") && s.contains("runtime initialization failed")
                })
        });
        assert!(
            init_error_visible,
            "runtime_init_error marker must still reach the LLM — it has no runtime-layer counterpart; messages: {messages:?}"
        );
    }

    /// A bare lifecycle-layer marker with NO adjacent runtime-layer
    /// bubble (e.g. corrupted DB, partial write, future code path that
    /// re-introduces the marker without a bubble) must be kept — the
    /// dedup filter only fires on the duplicate pattern, not on every
    /// `kind: "error"` `type: "run_boundary"` marker it sees.  This
    /// preserves the agent's ability to answer "why did that fail?"
    /// even when the runtime-layer write was lost.
    #[test]
    fn bare_run_boundary_marker_without_adjacent_bubble_kept() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "do something"),
            // A bare lifecycle-layer marker with NO preceding bubble.
            // Not the duplicate pattern — the filter must keep it.
            make_error_marker("(run failed) lonely marker", "failed", "lonely error"),
        ];

        let messages = builder.build("System.", &history, "why?", None);

        let marker_visible = messages.iter().any(|m| {
            m.role == "user"
                && m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("[Error]") && s.contains("lonely marker"))
        });
        assert!(
            marker_visible,
            "a bare run_boundary marker without an adjacent bubble must still surface as `[Error] ...` so the agent can answer follow-ups; messages: {messages:?}"
        );
    }
}
