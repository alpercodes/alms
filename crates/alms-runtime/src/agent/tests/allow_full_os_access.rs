// SPDX-License-Identifier: Apache-2.0

//! Tests for #947 — the `[security].allow_full_os_access` operator
//! escape hatch from the project-root sandbox. The runtime exposes
//! `AgentRuntime::with_unrestricted_filesystem()` which the gateway
//! invokes for listed agents in lieu of `with_project_root`.

use crate::agent::*;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::AgentId;

/// Build a runtime whose `AgentRuntime::new` resolved the
/// project-as-sandbox path. Mirrors what the gateway does for an
/// agent that is NOT on `allow_full_os_access`. This is the baseline
/// the test compares the unsandboxed variant against.
fn make_sandboxed_runtime(sandbox_root: std::path::PathBuf) -> AgentRuntime {
    let cfg = AgentConfig {
        sandbox_root: sandbox_root.to_string_lossy().into_owned(),
        shell_policy: "sandboxed".into(),
        ..AgentConfig::default()
    };
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    AgentRuntime::new(AgentId::new(), cfg, LlmClient::new(llm_config).unwrap()).unwrap()
}

/// `with_unrestricted_filesystem` clears `resolved_sandbox_root` so
/// every fs_* tool now operates without a path prefix to enforce.
/// This is the runtime-side proof of the issue's "listed agent:
/// `fs_read('/etc/passwd')` succeeds" acceptance criterion.
#[test]
fn clears_resolved_sandbox_root() {
    let sandbox = tempfile::tempdir().unwrap();
    let runtime = make_sandboxed_runtime(sandbox.path().to_path_buf());
    // Before: sandbox is active.
    assert!(
        runtime.resolved_sandbox_root.is_some(),
        "AgentRuntime::new must populate resolved_sandbox_root from \
         config.sandbox_root for an agent that is NOT on allow_full_os_access"
    );

    let runtime = runtime.with_unrestricted_filesystem();
    assert!(
        runtime.resolved_sandbox_root.is_none(),
        "with_unrestricted_filesystem must clear resolved_sandbox_root \
         so fs_* tools have no path prefix to enforce (#947)"
    );
    assert!(
        runtime.shell_unrestricted,
        "with_unrestricted_filesystem must set shell_unrestricted = true \
         so the runtime invariant `resolved_sandbox_root.is_none() ↔ \
         shell_unrestricted` is preserved"
    );
}

/// After `with_unrestricted_filesystem`, an `fs_read` of an
/// out-of-sandbox path must succeed (subject only to OS perms),
/// while the sandboxed baseline blocks the same path. This is the
/// load-bearing acceptance check from the issue: listed agent reads
/// outside project root, unlisted agent does not.
#[tokio::test]
async fn unsandboxed_fs_read_reaches_outside_sandbox() {
    // Create two sibling tempdirs: the sandbox root and a sibling
    // "outside" directory holding a file the agent must not be able
    // to read in the sandboxed case.
    let sandbox = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret_path = outside.path().join("outside.txt");
    std::fs::write(&secret_path, b"hidden\n").unwrap();

    // Baseline: sandboxed agent — fs_read of the outside path is rejected.
    let sandboxed = make_sandboxed_runtime(sandbox.path().to_path_buf());
    let result_blocked = sandboxed
        .tools
        .execute(
            "fs_read",
            serde_json::json!({ "path": secret_path.to_string_lossy() }),
        )
        .await;
    assert!(
        result_blocked.is_err(),
        "sandboxed agent must NOT be able to fs_read a path outside \
         the project-root sandbox — #945 boundary"
    );

    // Listed agent: fs_read of the same path succeeds.
    let unsandboxed =
        make_sandboxed_runtime(sandbox.path().to_path_buf()).with_unrestricted_filesystem();
    let result_ok = unsandboxed
        .tools
        .execute(
            "fs_read",
            serde_json::json!({ "path": secret_path.to_string_lossy() }),
        )
        .await
        .expect("listed agent must be able to fs_read outside the project sandbox");
    let body = result_ok
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    assert!(
        body.contains("hidden"),
        "fs_read should return the file's content for the listed agent — \
         got: {result_ok}"
    );
}

/// `shell_permissions` deny entries (#717) still apply to a listed
/// agent — `allow_full_os_access` only drops the project-root
/// filesystem boundary, not the operator's independent denylist
/// policy. This is the issue's
/// "shell_permissions deny entries still block listed agents"
/// acceptance check.
#[tokio::test]
async fn shell_permissions_deny_still_applies() {
    let sandbox = tempfile::tempdir().unwrap();
    let cfg = AgentConfig {
        sandbox_root: sandbox.path().to_string_lossy().into_owned(),
        shell_policy: "sandboxed".into(),
        shell_permissions: alms_core::config::ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec![r"^rm\b".into()],
            classifier_overrides: vec![],
        },
        // Disable the classifier floor so the test exercises the
        // shell_permissions denylist path specifically — without
        // this, `rm -rf /` would be blocked by the classifier
        // before the denylist check runs.
        shell_classification_mode: alms_core::config::ShellClassificationMode::Off,
        ..AgentConfig::default()
    };
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let runtime = AgentRuntime::new(AgentId::new(), cfg, LlmClient::new(llm_config).unwrap())
        .expect("AgentRuntime::new")
        .with_unrestricted_filesystem();

    let denied = runtime
        .tools
        .execute(
            "shell",
            serde_json::json!({ "command": "rm -rf /tmp/deleteme" }),
        )
        .await;
    // The shell tool surfaces the deny as either an Err or a
    // success-with-error-payload depending on how it normalises
    // policy violations. Either shape proves the denylist was
    // honoured — what we must NOT see is the command running.
    let denied_outcome = format!("{denied:?}");
    assert!(
        denied_outcome.to_lowercase().contains("denied")
            || denied_outcome.to_lowercase().contains("blocked")
            || denied_outcome.to_lowercase().contains("permission")
            || denied.is_err(),
        "shell_permissions denylist must block `rm -rf` even for an \
         agent on allow_full_os_access — got: {denied_outcome}"
    );
}

/// The destructive-command classifier (#745) still applies to a
/// listed agent — `allow_full_os_access` only drops the project-root
/// filesystem boundary, not the classifier floor. This is the
/// issue's acceptance criterion 5b ("classifier still blocks listed
/// agents") and the sibling check to
/// `shell_permissions_deny_still_applies`: that test isolates the
/// `shell_permissions` denylist by setting
/// `shell_classification_mode = Off`; this test isolates the
/// classifier by leaving the denylist empty and setting the
/// classifier to its default `BlockDestructive` mode. Together they
/// prove both independent operator-policy gates remain active for
/// an unsandboxed agent.
#[tokio::test]
async fn classifier_floor_still_blocks_unsandboxed_agent() {
    let sandbox = tempfile::tempdir().unwrap();
    let cfg = AgentConfig {
        sandbox_root: sandbox.path().to_string_lossy().into_owned(),
        shell_policy: "sandboxed".into(),
        // Empty permissions — no allowlist, no denylist — so the
        // classifier is the only thing standing between this
        // command and execution. The classifier mode is the
        // `BlockDestructive` default, made explicit here for the
        // test record.
        shell_permissions: alms_core::config::ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec![],
            classifier_overrides: vec![],
        },
        shell_classification_mode: alms_core::config::ShellClassificationMode::BlockDestructive,
        ..AgentConfig::default()
    };
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let runtime = AgentRuntime::new(AgentId::new(), cfg, LlmClient::new(llm_config).unwrap())
        .expect("AgentRuntime::new")
        .with_unrestricted_filesystem();

    // `rm -rf /etc` is the canonical destructive-command target the
    // classifier flags as `Destructive`. With
    // `allow_full_os_access` lifting the sandbox, the only thing
    // blocking this command is the classifier floor — exactly what
    // we are testing.
    let blocked = runtime
        .tools
        .execute("shell", serde_json::json!({ "command": "rm -rf /etc" }))
        .await;
    let blocked_outcome = format!("{blocked:?}");
    assert!(
        blocked_outcome.to_lowercase().contains("classifier")
            || blocked_outcome.to_lowercase().contains("blocked")
            || blocked_outcome.to_lowercase().contains("destructive")
            || blocked.is_err(),
        "destructive-command classifier must block `rm -rf /etc` \
         even for an agent on allow_full_os_access — got: \
         {blocked_outcome}"
    );
}

/// Worktree-mode sibling-reads guard test (#946).
///
/// When an agent runs in `WorktreeMode::Git`, the gateway pins the
/// sandbox at `<project>/.alms/worktrees/<name>/` instead of the
/// project root. The agent must STILL be able to read sibling
/// personality metadata at
/// `<project>/.alms/agents/<sibling>/personality.md`, which sits
/// OUTSIDE the worktree directory.
///
/// The gateway's run-lifecycle wiring achieves this by calling
/// `with_extra_fs_read_root(<project>/.alms/agents/)` BEFORE
/// `with_project_root(<worktree_path>)`. This test pins that
/// invariant: if a future refactor reorders the calls or drops
/// the extra read root, the parent agent loses access to sibling
/// personality and multi-agent coordination silently breaks.
#[tokio::test]
async fn test_worktree_mode_sibling_reads_via_extra_read_root() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");
    let worktree_dir = project_root.join(".alms").join("worktrees").join("parent");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&worktree_dir).unwrap();

    // Sibling metadata lives at `.alms/agents/sibling/` — OUTSIDE
    // the worktree dir, but reachable via the extra-read-roots
    // shim the gateway wires up.
    let sibling_meta_dir = agents_dir.join("sibling");
    std::fs::create_dir_all(&sibling_meta_dir).unwrap();
    let sibling_personality = sibling_meta_dir.join("personality.md");
    std::fs::write(&sibling_personality, "I am the sibling.\n").unwrap();

    // Build the parent agent exactly as the worktree-mode-git
    // path of the gateway run lifecycle wires it: extra read
    // root FIRST (sibling reads), then `with_project_root` at
    // the worktree, then `with_workspace` for the parent's own
    // metadata directory inside the project.
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
    .with_extra_fs_read_root(agents_dir.clone())
    .with_project_root(worktree_dir.clone())
    .with_workspace(AgentWorkspace::new(&agents_dir, "parent"));

    // Resolve the absolute path to the sibling's personality file.
    // We pass an absolute path because, from inside the worktree,
    // `.alms/agents/...` would resolve relative to the worktree
    // and miss the sibling tree entirely. The extra-read-roots
    // shim accepts the absolute path as long as it's inside one
    // of the registered extras.
    let abs =
        std::fs::canonicalize(&sibling_personality).expect("canonicalize sibling personality path");

    let result = runtime
        .tools()
        .execute(
            "fs_read",
            serde_json::json!({ "path": abs.to_string_lossy() }),
        )
        .await;

    assert!(
        result.is_ok(),
        "parent in worktree mode must be able to read sibling \
         personality via extra_fs_read_roots: {:?}",
        result.err(),
    );
    let value = result.unwrap();
    assert!(
        value["content"]
            .as_str()
            .unwrap_or("")
            .contains("I am the sibling"),
        "expected sibling personality contents, got {value}"
    );
}

/// Defensive: a parent in worktree mode WITHOUT the extra read
/// root cannot reach the sibling personality file. This pins
/// the load-bearing nature of the `with_extra_fs_read_root`
/// call in `runs/lifecycle.rs::execute_run` — drop the call and
/// the test passes (read fails) where it should fail (read
/// succeeds).
#[tokio::test]
async fn test_worktree_mode_sibling_reads_blocked_without_extras() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");
    let worktree_dir = project_root.join(".alms").join("worktrees").join("parent");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&worktree_dir).unwrap();

    let sibling_meta_dir = agents_dir.join("sibling");
    std::fs::create_dir_all(&sibling_meta_dir).unwrap();
    let sibling_personality = sibling_meta_dir.join("personality.md");
    std::fs::write(&sibling_personality, "I am the sibling.\n").unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    // No `with_extra_fs_read_root` call — only `with_project_root`
    // pinning the sandbox at the worktree dir.
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(worktree_dir.clone());

    let abs =
        std::fs::canonicalize(&sibling_personality).expect("canonicalize sibling personality path");

    let result = runtime
        .tools()
        .execute(
            "fs_read",
            serde_json::json!({ "path": abs.to_string_lossy() }),
        )
        .await;

    assert!(
        result.is_err() || result.as_ref().ok().and_then(|v| v.get("error")).is_some(),
        "parent without extra_fs_read_roots must NOT be able to \
         read outside the worktree — got Ok({:?})",
        result.ok(),
    );
}
