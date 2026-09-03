// SPDX-License-Identifier: Apache-2.0

//! Cross-tool parity for subagent transcript ownership (#1298).
//!
//! `read_session` and `read_subagent_session` both decide who may read a
//! subagent session, and before #1298 they answered differently for the
//! *same bytes*: `read_subagent_session` authorized on the parent embedded in
//! the `context_id`, while `read_session` authorized on `session.agent_id` —
//! which #1288 moved onto the invoked agent. One tool refused the invoked
//! agent its own subagent transcript and the other handed it over.
//!
//! These rows drive both tools' real entry points (`Tool::execute`), not the
//! access checks underneath them, and assert the answers match. The rule
//! itself is stated once in `alms_core::subagent_session_access`; this file
//! is the proof that both consumers actually go through it.
//!
//! Scope: *subagent* sessions. The tools deliberately differ elsewhere —
//! `read_subagent_session` bounces a non-subagent session to `read_session`
//! rather than serving it, which is routing, not authorization.

use alms_core::AgentId;
use alms_sandbox::Tool;
use alms_session::{Content, Message, Role, SessionConfig, SessionManager};
use alms_tools::{ReadSessionTool, ReadSubagentSessionTool};
use serde_json::Value;
use std::sync::Arc;

fn manager() -> Arc<SessionManager> {
    Arc::new(SessionManager::new(SessionConfig::default()))
}

fn message(text: &str) -> Message {
    Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: Role::Assistant,
        content: Content::Text(text.to_string()),
        timestamp: alms_core::Timestamp::now(),
        metadata: None,
    }
}

/// A subagent session as it is actually persisted post-#1288: filed under the
/// *invoked* agent's registry id, with the *parent* named in the context.
fn subagent_session(
    mgr: &Arc<SessionManager>,
    context_id: &str,
    filed_under: AgentId,
) -> alms_session::Session {
    let session = mgr.get_or_create(filed_under, context_id);
    mgr.append_message(session.id, message("reviewed it"))
        .unwrap();
    session
}

async fn via_read_session(
    mgr: &Arc<SessionManager>,
    reader: AgentId,
    session_id: alms_core::SessionId,
) -> Value {
    ReadSessionTool::new(mgr.clone(), reader, Some("reader".to_string()))
        .execute(serde_json::json!({ "session_id": session_id.0.to_string() }))
        .await
        .unwrap()
}

async fn via_read_subagent_session(
    mgr: &Arc<SessionManager>,
    reader: AgentId,
    session_id: alms_core::SessionId,
) -> Value {
    ReadSubagentSessionTool::new(mgr.clone(), reader)
        .execute(serde_json::json!({ "session_id": session_id.0.to_string() }))
        .await
        .unwrap()
}

/// Both tools return a JSON object either way, so "granted" is the absence of
/// an `error` key rather than a transport-level failure.
fn granted(result: &Value) -> bool {
    result.get("error").is_none()
}

/// The parity table. Every reader of interest, both tools, one expectation.
///
/// Flip either tool's answer and this fails: authorize `read_session` on
/// `session.agent_id` again and the invoked-agent row diverges; authorize
/// `read_subagent_session` on it and the parent row does.
#[tokio::test]
async fn both_tools_agree_on_who_owns_a_named_subagent_transcript() {
    let mgr = manager();
    let parent = AgentId::new();
    let invoked = AgentId::new();
    let context_id = alms_core::named_subagent_context_id(parent, "reviewer");
    let session = subagent_session(&mgr, &context_id, invoked);

    assert_eq!(
        session.agent_id, invoked,
        "test setup: #1288 files the row under the invoked agent"
    );

    for (who, reader, expected) in [
        ("the spawning parent", parent, true),
        ("the invoked agent, which owns the row", invoked, false),
        ("an unrelated bystander with the id", AgentId::new(), false),
    ] {
        let own = via_read_session(&mgr, reader, session.id).await;
        let sub = via_read_subagent_session(&mgr, reader, session.id).await;

        assert_eq!(
            granted(&own),
            expected,
            "read_session disagreed for {who}: {own}"
        );
        assert_eq!(
            granted(&sub),
            expected,
            "read_subagent_session disagreed for {who}: {sub}"
        );

        if expected {
            assert_eq!(own["messages"][0]["content"], "reviewed it");
            assert_eq!(sub["messages"][0]["content"], "reviewed it");
        } else {
            // Same refusal, in the same words — the denial text comes from
            // `SubagentAccessDenial`, not from each tool's own phrasing.
            assert_eq!(own["error"], sub["error"], "denial text diverged for {who}");
        }
    }
}

/// Ephemeral subagents are the same rule with a task id where the name goes.
#[tokio::test]
async fn both_tools_agree_on_an_ephemeral_subagent_transcript() {
    let mgr = manager();
    let parent = AgentId::new();
    let context_id = format!("subagent_{}_{}", parent.0, uuid::Uuid::new_v4());
    let session = subagent_session(&mgr, &context_id, AgentId::new());

    for (who, reader, expected) in [
        ("the spawning parent", parent, true),
        ("an unrelated bystander", AgentId::new(), false),
    ] {
        let own = via_read_session(&mgr, reader, session.id).await;
        let sub = via_read_subagent_session(&mgr, reader, session.id).await;
        assert_eq!(granted(&own), expected, "read_session for {who}: {own}");
        assert_eq!(
            granted(&sub),
            expected,
            "read_subagent_session for {who}: {sub}"
        );
    }
}

/// The #1185 hardening, held by both tools: a legacy `subagent_{task_id}`
/// records no parent and is refused to everyone. The reader here is the agent
/// the row is filed under — the one `read_session`'s `session.agent_id` check
/// would admit if the subagent branch were removed.
#[tokio::test]
async fn both_tools_deny_a_legacy_subagent_context_to_the_agent_it_is_filed_under() {
    let mgr = manager();
    let filed_under = AgentId::new();
    let context_id = format!("subagent_{}", uuid::Uuid::new_v4());
    let session = subagent_session(&mgr, &context_id, filed_under);

    let own = via_read_session(&mgr, filed_under, session.id).await;
    let sub = via_read_subagent_session(&mgr, filed_under, session.id).await;

    assert!(
        !granted(&own),
        "read_session served a legacy context: {own}"
    );
    assert!(
        !granted(&sub),
        "read_subagent_session served a legacy context: {sub}"
    );
    assert_eq!(own["error"], sub["error"]);
}
