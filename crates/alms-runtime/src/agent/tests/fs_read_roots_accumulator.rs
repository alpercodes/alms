// SPDX-License-Identifier: Apache-2.0

//! Tests for the #921 review fix #1 — `extra_fs_read_roots` accumulator
//! pattern. The pre-fix code re-registered fs_* tools inside both
//! `with_shell_spill` and `with_tool_output_truncate`, but `with_workspace`
//! (called LAST in the gateway lifecycle order) silently overwrote those
//! extras. The fix replaces the per-builder fs_* re-registration with a
//! single `extra_fs_read_roots` accumulator that every fs_* registration
//! site reads from.

use crate::agent::*;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use crate::workspace::AgentWorkspace;
use alms_core::AgentId;

fn make_runtime_with_sandbox(sandbox_root: std::path::PathBuf) -> AgentRuntime {
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

/// `with_workspace` followed by `with_shell_spill` and
/// `with_tool_output_truncate` must end up with the same accumulated
/// extras as the documented gateway order
/// (`with_shell_spill` → `with_tool_output_truncate` → `with_workspace`).
///
/// Pre-fix the documented order silently dropped the spill extras
/// because `with_workspace` overwrote them. Post-fix the accumulator
/// is the single source of truth and the order does not matter.
#[test]
fn extras_survive_either_call_order() {
    let sandbox = tempfile::tempdir().unwrap();
    // Create the workspace as a subdirectory so canonicalize() resolves.
    let ws_dir = sandbox.path().join("agent");
    std::fs::create_dir_all(&ws_dir).unwrap();
    let shell_dir = sandbox.path().join("shell_output").join("run-1");
    let trunc_dir = sandbox.path().join("tool-output").join("run-1");
    std::fs::create_dir_all(&shell_dir).unwrap();
    std::fs::create_dir_all(&trunc_dir).unwrap();

    // Order A: spill → truncate → workspace (the documented gateway order)
    let runtime_a = make_runtime_with_sandbox(sandbox.path().to_path_buf())
        .with_shell_spill(shell_dir.clone(), true)
        .with_tool_output_truncate(trunc_dir.clone(), true, 32 * 1024, 2000)
        .with_workspace(AgentWorkspace::with_dir(ws_dir.clone()));

    // Order B: workspace first, then the spill builders. Pre-fix this
    // would have produced a different (broader) extras set than A
    // because workspace's overwrite of fs_* tools dropped the trunc
    // dir. Post-fix the accumulator collects every spill dir
    // regardless of when the workspace registration happened.
    let runtime_b = make_runtime_with_sandbox(sandbox.path().to_path_buf())
        .with_workspace(AgentWorkspace::with_dir(ws_dir.clone()))
        .with_shell_spill(shell_dir.clone(), true)
        .with_tool_output_truncate(trunc_dir.clone(), true, 32 * 1024, 2000);

    // Both runtimes must have both spill dirs in their accumulator,
    // regardless of call order. (We compare as a sorted set so a
    // future re-ordering of the push sites doesn't break the test.)
    let mut a: Vec<_> = runtime_a.extra_fs_read_roots.iter().collect();
    let mut b: Vec<_> = runtime_b.extra_fs_read_roots.iter().collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "spill extras must be order-independent");
    assert!(
        a.iter()
            .any(|p| p.ends_with("shell_output/run-1") || p.ends_with("shell_output\\run-1")),
        "shell_output spill dir must be in extras: {:?}",
        a
    );
    assert!(
        a.iter()
            .any(|p| p.ends_with("tool-output/run-1") || p.ends_with("tool-output\\run-1")),
        "tool-output spill dir must be in extras: {:?}",
        a
    );
}
