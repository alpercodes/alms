// SPDX-License-Identifier: Apache-2.0

//! Sandbox-root resolution and the v2 project-root layout shared by `fs_*` and `shell`.

use crate::agent::*;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::AgentId;

#[test]
fn test_invalid_sandbox_root_fails_closed() {
    let config = crate::llm_types::LlmConfig {
        mock: true,
        ..crate::llm_types::LlmConfig::default()
    };
    let llm = LlmClient::new(config).unwrap();
    let agent_config = AgentConfig {
        sandbox_root: "/nonexistent/path/that/does/not/exist".into(),
        ..AgentConfig::default()
    };
    let result = AgentRuntime::new(AgentId::new(), agent_config, llm);
    assert!(result.is_err(), "Should fail when sandbox_root is invalid");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sandbox_root"),
        "Error should mention sandbox_root: {err}"
    );
}

#[test]
fn test_empty_sandbox_root_means_unrestricted() {
    let config = crate::llm_types::LlmConfig {
        mock: true,
        ..crate::llm_types::LlmConfig::default()
    };
    let llm = LlmClient::new(config).unwrap();
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let result = AgentRuntime::new(AgentId::new(), agent_config, llm);
    assert!(
        result.is_ok(),
        "Empty sandbox_root should mean unrestricted"
    );
}

/// Sibling-workspace reads guard test (#242, restated for the v2
/// project-root-as-sandbox model — #945).
///
/// Scenario: a project root contains the standard v2 layout
/// `<project>/.alms/agents/<name>/personality.md`. A parent agent is
/// constructed with `with_project_root(<project>)` and
/// `with_workspace(<agents_dir>/parent/)` — mirroring exactly what the
/// gateway does on a `POST /runs`. The parent must still be able to
/// `fs_read('.alms/agents/<sibling>/personality.md')` for a sibling
/// agent's metadata.
///
/// This is the load-bearing #242 invariant: child agents must be able
/// to read parent personality and vice versa for multi-agent
/// coordination. Pre-#945 it was satisfied by `with_workspace`'s
/// `extra_read_roots = [parent_of_workspace]` shim. After #945 it is
/// satisfied "by accident" — every agent's metadata lives at
/// `<project>/.alms/agents/<name>/`, naturally inside the project-root
/// sandbox, so no extras are needed at the runtime layer. Tim's v2
/// proposal explicitly named this as a load-bearing invariant; if a
/// future refactor breaks it, child agents will silently lose access
/// to parent metadata.
#[tokio::test]
async fn test_sibling_workspace_reads_under_project_root_v2() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    // Parent and sibling metadata live under `.alms/agents/`, the v2
    // canonical layout. The sandbox root for the parent agent is the
    // project root, so both metadata directories sit inside it.
    let parent_meta_dir = agents_dir.join("parent");
    let sibling_meta_dir = agents_dir.join("sibling");
    std::fs::create_dir_all(&parent_meta_dir).unwrap();
    std::fs::create_dir_all(&sibling_meta_dir).unwrap();

    // Write a personality.md file in the sibling's metadata directory.
    let sibling_personality = sibling_meta_dir.join("personality.md");
    std::fs::write(&sibling_personality, "I am a helpful sibling agent.\n").unwrap();

    // Build the parent agent: project-root sandbox + workspace
    // attachment, exactly as the gateway wires it.
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        // Empty sandbox_root keeps `AgentRuntime::new` from canonicalising
        // a stray "." against the test's cwd; `with_project_root` below
        // is the single source of truth for the sandbox root in v2.
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone())
    .with_workspace(AgentWorkspace::new(&agents_dir, "parent"));

    // The parent agent reads the sibling's personality.md via a
    // project-relative path — exactly the canonical v2 path shape from
    // the issue's acceptance criterion: `.alms/agents/<sibling>/personality.md`.
    let result = runtime
        .tools()
        .execute(
            "fs_read",
            serde_json::json!({ "path": ".alms/agents/sibling/personality.md" }),
        )
        .await;
    assert!(
        result.is_ok(),
        "parent must be able to read sibling's personality.md under v2 project-root sandbox: {:?}",
        result.err()
    );
    let value = result.unwrap();
    assert!(
        value["content"]
            .as_str()
            .unwrap_or("")
            .contains("helpful sibling agent"),
        "expected sibling's personality content, got {}",
        value
    );
}

/// Acceptance pin: project-relative `fs_read` and `fs_write` operate
/// against the project directory itself (#945).
///
/// This is the issue's first acceptance bullet:
///
/// > Default install with no `alms.toml` knobs: `fs_read('./src/main.rs')`
/// > works and `shell('cat src/main.rs')` works, when the gateway was
/// > started in the project dir.
///
/// We exercise the fs_read / fs_write half here (the shell half is
/// platform-specific and gets covered in a separate sandbox unit test
/// rather than in this runtime-level integration test). The test
/// constructs a project tree on a tempdir, builds the runtime exactly
/// as the gateway does (`with_project_root` then `with_workspace`),
/// and confirms:
///   1. `fs_read('./src/main.rs')` returns the project file's contents.
///   2. `fs_write('./src/new.rs', ...)` lands the bytes at
///      `<project>/src/new.rs` — NOT under any `.alms/` subdirectory.
#[tokio::test]
async fn test_project_relative_fs_read_and_fs_write_v2() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(project_root.join("src")).unwrap();

    // Drop a real project file at `<project>/src/main.rs` so fs_read
    // has something to find via a project-relative path.
    let main_rs = project_root.join("src").join("main.rs");
    std::fs::write(&main_rs, "fn main() { println!(\"hi\"); }\n").unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone())
    .with_workspace(AgentWorkspace::new(&agents_dir, "alice"));

    // (1) fs_read('./src/main.rs') resolves under the project root.
    let read_result = runtime
        .tools()
        .execute("fs_read", serde_json::json!({ "path": "src/main.rs" }))
        .await
        .expect("fs_read should succeed");
    assert!(
        read_result["content"]
            .as_str()
            .unwrap_or("")
            .contains("println!"),
        "expected main.rs body, got {read_result}"
    );

    // The fs_write tool is read-before-write guarded by FileStateCache
    // (#273 / read-before-write contract). The fs_read above primes the
    // cache so the subsequent fs_write to a project-relative new file
    // path is allowed.
    let new_rs = project_root.join("src").join("new.rs");
    let write_result = runtime
        .tools()
        .execute(
            "fs_write",
            serde_json::json!({
                "path": "src/new.rs",
                "content": "// generated by agent\n",
            }),
        )
        .await
        .expect("fs_write should succeed");
    assert!(
        write_result.get("ok").and_then(|v| v.as_bool()) == Some(true)
            || write_result.get("path").is_some(),
        "fs_write returned an unexpected payload shape: {write_result}"
    );

    // (2) The bytes landed at the project location, not under `.alms/`.
    let on_disk = std::fs::read_to_string(&new_rs)
        .expect("fs_write must have created `<project>/src/new.rs`");
    assert!(on_disk.contains("generated by agent"));

    // Defensive: nothing should have been created under `.alms/` for
    // this write — that path was the v1 sandbox shape.
    assert!(
        !agents_dir.join("alice").join("src").exists(),
        "fs_write must not have routed bytes under `.alms/agents/<name>/`"
    );
}

/// Acceptance pin: `fs_*` and `shell` agree on the same sandbox root
/// after `with_project_root` (#945).
///
/// This covers the issue's last acceptance bullet:
///
/// > Audit-log payloads on `fs_*` and `shell` agree on the sandbox root
/// > they're enforcing against.
///
/// We can't easily inspect the audit-log payload from a unit test, but
/// we can pin the upstream invariant: after `with_project_root`, the
/// runtime's `resolved_sandbox_root` field — which is the single
/// source of truth threaded into both the fs_* tools' sandbox check
/// and the shell tool's `with_policy` constructor — points at the
/// canonicalised project root. If a future refactor splits the two
/// roots back apart, this assertion fires.
#[tokio::test]
async fn test_fs_and_shell_share_one_sandbox_root_v2() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone());

    let canonical_project = std::fs::canonicalize(&project_root).unwrap();
    assert_eq!(
        runtime.resolved_sandbox_root.as_deref(),
        Some(canonical_project.as_path()),
        "after with_project_root, the runtime's single sandbox root must \
         match the canonicalised project root — fs_* and shell both read \
         from this field at registration time"
    );
}

/// Acceptance pin: `workspace_write` lands files at
/// `<project>/.alms/agents/<name>/personality.md` (#945).
///
/// This is the issue's third acceptance bullet:
///
/// > `<project>/.alms/agents/<name>/personality.md` is the canonical
/// > metadata location; `workspace_write` still writes there.
#[tokio::test]
async fn test_workspace_write_lands_under_alms_agents_path_v2() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone())
    .with_workspace(AgentWorkspace::new(&agents_dir, "alice"));

    // `workspace_write` is the one tool that targets agent metadata
    // files specifically. It writes via `AgentWorkspace::write_file`,
    // which puts the file at `<workspace_dir>/<name>/<filename>`.
    let result = runtime
        .tools()
        .execute(
            "workspace_write",
            serde_json::json!({
                "file": "personality",
                "content": "I am Alice and I help with code.\n",
                "mode": "write",
            }),
        )
        .await
        .expect("workspace_write should succeed");
    assert_eq!(result["ok"], serde_json::json!(true), "result: {result}");

    // The canonical v2 location is `<project>/.alms/agents/<name>/personality.md`.
    let canonical = project_root
        .join(".alms")
        .join("agents")
        .join("alice")
        .join("personality.md");
    let on_disk = std::fs::read_to_string(&canonical)
        .expect("workspace_write must land at `<project>/.alms/agents/<name>/personality.md`");
    assert!(on_disk.contains("I am Alice"));
}

/// Sandbox-escape guard: paths outside the project root are still
/// rejected for `fs_write` even under the v2 single-root model (#945).
///
/// Pre-#945 there was a tighter "writes are confined to the workspace
/// dir, reads can extend to siblings via extras" split. v2 collapses
/// to a single sandbox root — the project root — so writes to sibling
/// metadata at `.alms/agents/<sibling>/personality.md` are *allowed*
/// (they're inside the project root) and that is the intended new
/// behaviour. What still must NOT work is escaping the project root
/// entirely; this test pins that invariant for `fs_write`.
#[tokio::test]
async fn test_v2_fs_write_rejects_paths_outside_project_root() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    // Two sibling tempdirs: one is the project root, the other is
    // explicitly outside it. fs_write to the outside dir must fail.
    let project_dir = tempdir().unwrap();
    let outside_dir = tempdir().unwrap();
    let project_root = project_dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone())
    .with_workspace(AgentWorkspace::new(&agents_dir, "parent"));

    // fs_write to a path outside the project root must fail — the
    // project-root sandbox is the single source of truth in v2.
    let outside_target = outside_dir.path().join("escape.txt");
    let result = runtime
        .tools()
        .execute(
            "fs_write",
            serde_json::json!({
                "path": outside_target.to_str().unwrap(),
                "content": "should not land",
            }),
        )
        .await;
    assert!(
        result.is_err(),
        "fs_write must reject paths outside the project-root sandbox"
    );
    assert!(
        !outside_target.exists(),
        "the file must not have been created"
    );
}

/// Ephemeral subagent v2 behaviour: shares the parent's project-root
/// sandbox (#945). Pre-#945 the runtime used the ephemeral workspace
/// dir as the sandbox root and therefore *could not* read a sibling
/// named-agent's metadata; v2 unifies on the project root, so an
/// ephemeral subagent CAN read sibling metadata at
/// `<project>/.alms/agents/<name>/personality.md` — same allowance the
/// parent itself has. This test documents the v2 semantics so a future
/// refactor that tightens isolation cannot silently land without
/// updating the surrounding contracts (#946 is the proper place to
/// reintroduce per-subagent isolation as an opt-in).
#[tokio::test]
async fn test_ephemeral_subagent_inherits_project_root_sandbox_v2() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");

    // Named agent lives at `<project>/.alms/agents/researcher/`.
    let named_meta_dir = agents_dir.join("researcher");
    std::fs::create_dir_all(&named_meta_dir).unwrap();
    let named_memories = named_meta_dir.join("memories.md");
    std::fs::write(&named_memories, "researcher notes\n").unwrap();

    // Ephemeral subagent metadata lives at
    // `<project>/.alms/agents/.ephemeral/<task_id>/` — flat under the
    // agents dir. Its sandbox is the project root, same as any other
    // subagent.
    let task_id = "test-task-00000000";
    let ephemeral_meta_dir = agents_dir.join(".ephemeral").join(task_id);
    std::fs::create_dir_all(&ephemeral_meta_dir).unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone())
    .with_workspace(AgentWorkspace::with_dir(ephemeral_meta_dir.clone()));

    // Under v2 the ephemeral subagent CAN read the named agent's
    // memories.md — both live inside the same project-root sandbox.
    let result = runtime
        .tools()
        .execute(
            "fs_read",
            serde_json::json!({
                "path": ".alms/agents/researcher/memories.md",
            }),
        )
        .await;
    assert!(
        result.is_ok(),
        "ephemeral subagent should inherit the project-root sandbox in v2: {:?}",
        result.err()
    );
}
