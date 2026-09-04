// SPDX-License-Identifier: Apache-2.0

//! #1310 — the shown-view guard for workspace files, wired into a run.

use crate::agent::*;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::AgentId;
use alms_session::{SessionConfig, SessionManager};

// ── #1310: the shown-view guard, wired into a run ──────────────────────────

/// Build a mock runtime with `workspace` attached, the way the gateway does.
fn runtime_with_workspace(workspace: &crate::workspace::AgentWorkspace) -> AgentRuntime {
    AgentRuntime::new(
        AgentId::new(),
        AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        },
        LlmClient::new(LlmConfig {
            mock: true,
            ..LlmConfig::default()
        })
        .unwrap(),
    )
    .expect("runtime")
    .with_workspace(workspace.clone())
}

/// The tool-loop rebuild re-reads the workspace, so an agent that appends in
/// one tool batch and replaces in the next is judged against what it can
/// actually see.
///
/// This is a correction to the premise #1305 and #1310 were both written on
/// — "`build_context` runs once per run, so the memories snapshot in the
/// system prompt is fixed at run start". It is fixed only until the first
/// tool call: `agent_loop` calls `rebuild_system_prompt_for_tool_loop` after
/// every batch, which goes through `assemble_system_prompt` ->
/// `build_system_prompt_prefix` and reads the files again, replacing
/// `messages[0]` outright so the stale copy is not even in the array any
/// more.
///
/// It matters here because it decides how far the guard should reach. The
/// window between the rebuild and the next replacement is a real one — a
/// single tool batch containing both an append and a `mode: "write"` never
/// sees a rebuild in between, and neither does a concurrent writer — but a
/// guard that refused *every* mid-run replacement would refuse this one too,
/// where the agent is looking straight at the file. Both halves are asserted:
/// refused before the rebuild, written after, and the rebuilt prompt actually
/// containing the appended entry, which is what makes the second half
/// legitimate rather than merely permissive.
#[tokio::test]
async fn the_tool_loop_rebuild_re_shows_the_workspace_and_unblocks_the_next_replacement() {
    use crate::workspace::{AgentWorkspace, CheckedWrite, RefusedWrite, WorkspaceFile};

    let dir = tempfile::tempdir().unwrap();
    let workspace = AgentWorkspace::new(dir.path(), "alice");
    workspace
        .write_file_as_operator(WorkspaceFile::Memories, "- M1\n- M2")
        .unwrap();
    let runtime = runtime_with_workspace(&workspace);

    // The run's first system prompt, assembled before the agent gets a turn.
    let initial = runtime.assemble_system_prompt(&runtime.config.system_prompt, true);
    assert!(
        initial.contains("- M2"),
        "precondition: memories are injected"
    );
    assert!(
        !initial.contains("- M3"),
        "precondition: M3 does not exist yet"
    );

    // The agent appends inside the run.
    workspace
        .append_file(WorkspaceFile::Memories, "- M3")
        .unwrap();

    assert_eq!(
        workspace
            .write_file_checked(WorkspaceFile::Memories, "- M1\n- M2, merged")
            .unwrap(),
        CheckedWrite::Refused(RefusedWrite::ChangedSinceShown),
        "before any rebuild, the agent's only copy of the file predates its own append"
    );

    // The rebuild `agent_loop` performs after a tool batch.
    let mut messages = vec![LlmMessage::system(initial)];
    runtime.rebuild_system_prompt_for_tool_loop(&mut messages, true, None);
    let rebuilt = messages[0].content.as_deref().unwrap();
    assert!(
        rebuilt.contains("- M3"),
        "the rebuild re-reads memories.md, so the appended entry is back in the prompt"
    );

    assert_eq!(
        workspace
            .write_file_checked(WorkspaceFile::Memories, "- M1\n- M2\n- M3, merged")
            .unwrap(),
        CheckedWrite::Written,
        "after the rebuild the agent has been re-shown the file, so replacing it is an \
         informed choice and must not be refused"
    );
}

/// `build_context` starts each run with no record of what the agent has been
/// shown, and the prompt assembly immediately refills it.
///
/// The two rows are the two ways to get this wrong. Without the reset, a
/// user-facing run's view of `user.md` outlives its context and authorises a
/// blind replacement in the DM run that follows — the file is injected only
/// for user-facing contexts, so the second run's agent has no copy of it.
/// With the reset in the wrong place (after the prompt assembly rather than
/// before), every replacement in every run is refused instead.
#[tokio::test]
async fn build_context_scopes_the_shown_record_to_one_run() {
    use crate::workspace::{AgentWorkspace, CheckedWrite, RefusedWrite, WorkspaceFile};

    for (label, second_context, expected) in [
        (
            "a second user-facing run re-shows user.md",
            "webchat-2",
            CheckedWrite::Written,
        ),
        (
            "a DM run does not, and cannot inherit the first run's view",
            "dm:alice:bob",
            CheckedWrite::Refused(RefusedWrite::NeverShown),
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = AgentWorkspace::new(dir.path(), "alice");
        workspace
            .write_file_as_operator(WorkspaceFile::User, "Name: Alper")
            .unwrap();
        let runtime = runtime_with_workspace(&workspace);
        let session_manager = SessionManager::new(SessionConfig::default());

        for context_id in ["webchat-1", second_context] {
            let session = session_manager.get_or_create(runtime.agent_id, context_id);
            runtime
                .build_context(&session_manager, &session.id, context_id, "hi")
                .await
                .unwrap();
        }

        assert_eq!(
            workspace
                .write_file_checked(WorkspaceFile::User, "Name: Someone Else")
                .unwrap(),
            expected,
            "{label}"
        );
    }
}

/// The two workspace tools are registered from one workspace, so a
/// `workspace_read` actually unblocks the `workspace_write` that was refused.
///
/// Asserted through `runtime.tools()` rather than on the tool structs,
/// because the thing that can break is the wiring: `with_workspace` cloning
/// two independent `AgentWorkspace` values instead of one would leave each
/// tool with its own record, and the sequence below would refuse twice.
#[tokio::test]
async fn the_registered_workspace_tools_share_one_view_of_the_files() {
    use crate::workspace::{AgentWorkspace, MEMORIES_INJECTION_CAP, WorkspaceFile};

    let dir = tempfile::tempdir().unwrap();
    let workspace = AgentWorkspace::new(dir.path(), "alice");
    let mut memories = String::new();
    let mut n = 0;
    while memories.len() < MEMORIES_INJECTION_CAP + 500 {
        memories.push_str(&format!("- entry {n}\n"));
        n += 1;
    }
    workspace
        .write_file_as_operator(WorkspaceFile::Memories, &memories)
        .unwrap();
    let runtime = runtime_with_workspace(&workspace);
    let _ = runtime.assemble_system_prompt(&runtime.config.system_prompt, true);

    let write_params = serde_json::json!({
        "file": "memories",
        "content": "- everything, compacted",
        "mode": "write",
    });

    let refused = runtime
        .tools()
        .execute("workspace_write", write_params.clone())
        .await
        .expect("a refusal is an in-band result, not a tool error");
    assert_eq!(refused["refused"], "shown_partially", "{refused}");

    let read = runtime
        .tools()
        .execute("workspace_read", serde_json::json!({"file": "memories"}))
        .await
        .expect("workspace_read must be registered alongside workspace_write");
    assert_eq!(read["content"].as_str().unwrap(), memories);

    // The rebuild `agent_loop` performs after every tool batch, and the step
    // that makes this sequence production-shaped rather than merely
    // tool-shaped. The read and the write CANNOT be one batch -- the model
    // has to see the read result to compose the replacement -- so exactly one
    // rebuild always lands between them. Without it this test passed while
    // the recovery it describes was unreachable in production (Tim, #1314).
    let _ = runtime.assemble_system_prompt(&runtime.config.system_prompt, true);

    let written = runtime
        .tools()
        .execute("workspace_write", write_params)
        .await
        .unwrap();
    assert_eq!(written["ok"], true, "{written}");
    assert_eq!(
        workspace.read_file(WorkspaceFile::Memories).unwrap(),
        "- everything, compacted"
    );
}
