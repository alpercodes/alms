// SPDX-License-Identifier: Apache-2.0

//! #1003 — `ContextDebug` emission and its agent attribution.

use super::base_runtime;
use crate::agent::*;
use crate::events::RuntimeEvent;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::AgentId;

// ──────────────────────────────────────────────────────────────────────
// #1003 — Restored Debug mode: ContextDebug emit attribution
//
// Previously, the runtime emitted `ContextDebug` events only with the
// payload (messages, tool_names, token counts). #1003 adds `agent_id`
// and `agent_name` so the UI can attribute each snapshot to the
// agent whose perspective produced it. This matters most for DM
// sessions where two agents alternate turns on the same session and
// each emits independent per-perspective context windows — without
// attribution, back-to-back panels are indistinguishable.
//
// The plumbing splits into two slices:
//
//   1. `AgentRuntime::emit_context_debug` reads `self.agent_id` /
//      `self.agent_name` and stuffs them into the event. This is
//      pure plumbing and the tests below pin both fields.
//
//   2. The agent loop's `if self.config.debug_mode` gate decides
//      whether to call emit at all. Per-agent `debug_mode` is layered
//      onto the resolved `AgentConfig` in the gateway's
//      `resolve_agent_config`; subagents see the parent's flag
//      through inheritance and the coordinator filters their emits
//      from the parent's SSE stream. The gateway-side merge is tested
//      in `crates/alms-gateway/src/agents.rs::tests` and
//      `crates/alms-session/src/sqlite/agents.rs::tests`; the runtime
//      side is what these tests cover.
//
// DM-perspective coverage: a DM session triggers `run_on_session`,
// which calls `build_context()` (with DM perspective mapping —
// `crates/alms-runtime/src/context/perspective.rs`) and reaches
// `finish_run`, which is where the `if self.config.debug_mode`
// emit gate fires. The same `emit_context_debug` runs for both the
// `run()` (webchat) and `run_on_session()` (DM) entry points, so
// pinning the attribution shape here covers both surfaces by
// construction. The gateway-level golden test in
// `crates/alms-gateway/tests/sse_golden_tests.rs` pins the SSE wire
// shape that the UI consumes.
fn build_test_runtime_for_emit(
    agent_id: AgentId,
    agent_name: Option<String>,
    event_sender: tokio::sync::mpsc::UnboundedSender<RuntimeEvent>,
) -> AgentRuntime {
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    AgentRuntime {
        agent_id,
        config: AgentConfig {
            // debug_mode is irrelevant for the helper-level tests
            // below — they call `emit_context_debug` directly. The
            // run-loop-level gate is exercised at the gateway layer.
            debug_mode: true,
            ..AgentConfig::default()
        },
        event_sender: Some(event_sender),
        agent_name,
        ..base_runtime(LlmClient::new(config).unwrap())
    }
}

#[tokio::test]
async fn emit_context_debug_carries_agent_id_and_name() {
    // Pin the wire-shape contract: `agent_id` is always populated
    // from `self.agent_id`, `agent_name` is populated from
    // `self.agent_name` (which is `Some` for named agents from the
    // registry, `None` for legacy unnamed runtimes).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let agent_id = AgentId::new();
    let runtime = build_test_runtime_for_emit(agent_id, Some("alpha".to_string()), tx);

    let messages = vec![
        LlmMessage::system("You are alpha."),
        LlmMessage::user("ping"),
    ];
    runtime.emit_context_debug(&messages);

    let event = rx
        .recv()
        .await
        .expect("emit must send a ContextDebug event");
    match event {
        RuntimeEvent::ContextDebug {
            agent_id: emitted_id,
            agent_name: emitted_name,
            history_message_count,
            ..
        } => {
            assert_eq!(
                emitted_id,
                agent_id.0.to_string(),
                "emitted agent_id must match self.agent_id"
            );
            assert_eq!(
                emitted_name.as_deref(),
                Some("alpha"),
                "emitted agent_name must match self.agent_name"
            );
            // `history_message_count` is `total - 1 (system) - 1 (last
            // user)` = 0 for this 2-message snapshot. Sanity check.
            assert_eq!(history_message_count, 0);
        }
        _ => panic!("Expected ContextDebug, got a different RuntimeEvent variant"),
    }
}

#[tokio::test]
async fn emit_context_debug_serializes_agent_name_as_null_for_unnamed_runtime() {
    // Legacy unnamed runtimes (no per-agent record, e.g. in-memory
    // tests or pre-registry deployments) carry `agent_name = None`.
    // The wire shape must serialise the field as `null` so the UI
    // can fall back to a placeholder label rather than treat the
    // missing field as an error. This pins both the `RuntimeEvent`
    // structure and the downstream JSON shape.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let agent_id = AgentId::new();
    let runtime = build_test_runtime_for_emit(agent_id, None, tx);

    runtime.emit_context_debug(&[LlmMessage::user("hi")]);

    let event = rx
        .recv()
        .await
        .expect("emit must send a ContextDebug event");
    match event {
        RuntimeEvent::ContextDebug {
            agent_id: emitted_id,
            agent_name: emitted_name,
            ..
        } => {
            assert_eq!(emitted_id, agent_id.0.to_string());
            assert!(
                emitted_name.is_none(),
                "unnamed runtime must emit agent_name = None"
            );
        }
        _ => panic!("Expected ContextDebug, got a different RuntimeEvent variant"),
    }
}

#[tokio::test]
async fn emit_context_debug_attributes_dm_perspective_to_each_agent() {
    // DM-perspective scenario: two agents (alpha + beta) alternate
    // turns on a shared DM session. Each agent's runtime fires
    // `emit_context_debug` during its own turn, populating
    // `agent_id` / `agent_name` from its own runtime fields.
    // The UI uses the pair to label per-turn panels so back-to-back
    // emits for the same DM session are visually unambiguous.
    //
    // Pinning here means a future refactor that accidentally drops
    // attribution from DM emits (e.g. by sharing a single emit
    // helper across runtimes) regresses this test instead of
    // silently breaking the UI's DM Debug panel.
    let alpha_id = AgentId::new();
    let beta_id = AgentId::new();

    let (tx_alpha, mut rx_alpha) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let (tx_beta, mut rx_beta) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();

    let alpha = build_test_runtime_for_emit(alpha_id, Some("alpha".to_string()), tx_alpha);
    let beta = build_test_runtime_for_emit(beta_id, Some("beta".to_string()), tx_beta);

    // Alpha's turn — perspective-mapped messages (alpha sees its
    // own outgoing as assistant; the DM mapper does the role
    // translation, but the emit contract doesn't care about the
    // payload here, only the attribution).
    alpha.emit_context_debug(&[
        LlmMessage::system("You are alpha. The peer 'beta' is in this DM."),
        LlmMessage::assistant("hi beta"),
        LlmMessage::user("hello alpha"),
    ]);

    // Beta's turn — its perspective sees the same conversation
    // mirrored, but emits attributed to beta.
    beta.emit_context_debug(&[
        LlmMessage::system("You are beta. The peer 'alpha' is in this DM."),
        LlmMessage::user("hi beta"),
        LlmMessage::assistant("hello alpha"),
    ]);

    let alpha_event = rx_alpha
        .recv()
        .await
        .expect("alpha must emit a ContextDebug event");
    let beta_event = rx_beta
        .recv()
        .await
        .expect("beta must emit a ContextDebug event");

    let (alpha_emitted_id, alpha_emitted_name) = match alpha_event {
        RuntimeEvent::ContextDebug {
            agent_id,
            agent_name,
            ..
        } => (agent_id, agent_name),
        _ => panic!("alpha: expected ContextDebug, got a different RuntimeEvent variant"),
    };
    let (beta_emitted_id, beta_emitted_name) = match beta_event {
        RuntimeEvent::ContextDebug {
            agent_id,
            agent_name,
            ..
        } => (agent_id, agent_name),
        _ => panic!("beta: expected ContextDebug, got a different RuntimeEvent variant"),
    };

    // Each emit lands on its own runtime's channel — alpha never
    // appears on beta's stream and vice versa.
    assert_eq!(alpha_emitted_id, alpha_id.0.to_string());
    assert_eq!(alpha_emitted_name.as_deref(), Some("alpha"));
    assert_eq!(beta_emitted_id, beta_id.0.to_string());
    assert_eq!(beta_emitted_name.as_deref(), Some("beta"));

    // Cross-talk check: the UI relies on the IDs being distinct so
    // it can correctly group panels per perspective. If a future
    // refactor accidentally inverts the attribution (e.g. emits the
    // peer's name instead of self's), this assertion catches it.
    assert_ne!(
        alpha_emitted_id, beta_emitted_id,
        "DM perspective emits must carry distinct agent IDs per turn"
    );
}
