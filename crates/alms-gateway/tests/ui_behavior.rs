//! Behaviour tests for the embedded `static/ui/` modules.
//!
//! The parse-sweep in `static_assets_parse.rs` only confirms each JS file is
//! syntactically valid. This file plugs the next gap: behavioural checks for
//! pure-logic modules that are exercised on every page load (history parsing,
//! tool-summary formatting, etc.). The tests themselves are written in JS
//! under `tests/ui/` and run via Node's built-in `node:test` runner — one
//! Rust test per JS test file shells out to `node --test <file>` and asserts
//! the run succeeded.
//!
//! When `node` is unavailable on the build machine, the harness skips with a
//! warning rather than failing — a behavioural test that depends on an
//! external interpreter shouldn't break Rust CI on machines that lack it.
//! GitHub Actions runners (and Iris's local Windows box) ship Node by
//! default, so the test does run there.
//!
//! See issue #898 for the regression that motivated this harness — the DM
//! reload path was silently dropping extended-thinking text and there was no
//! JS-level coverage to pin the new branch.

use std::path::PathBuf;
use std::process::Command;

/// Locate the JS test file relative to the crate manifest dir.
fn ui_test_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("ui")
        .join(name)
}

/// True when `node --version` succeeds. Used to gate the behaviour tests so
/// the suite still passes on machines without Node installed.
fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a JS test file under `node --test` and assert success. Captures the
/// child's stdout/stderr verbatim so failed assertions surface in the cargo
/// test output.
fn run_node_test(file: &str) {
    if !node_available() {
        eprintln!(
            "skipping ui_behavior::{}: `node` is not on PATH (install Node.js >= 22)",
            file,
        );
        return;
    }

    let path = ui_test_path(file);
    assert!(
        path.is_file(),
        "expected JS test file at {}",
        path.display(),
    );

    let output = Command::new("node")
        .arg("--test")
        .arg(&path)
        .output()
        .expect("failed to spawn `node --test`");

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!(
            "node --test {} failed (exit code {:?})\n\
             ---- stdout ----\n{}\n\
             ---- stderr ----\n{}",
            path.display(),
            output.status.code(),
            stdout,
            stderr,
        );
    }
}

/// Pinned regression for issue #898: on reload, the DM `dm_reasoning_text`
/// branch must read `metadata.reasoning_blocks` (the extended-thinking trace)
/// rather than `m.content` (the visible reply text) — otherwise reasoning-
/// capable models lose their chain-of-thought every time the page reloads.
///
/// Also covers the #767 sanity branch (regular non-DM agent messages still
/// expose `reasoning_blocks` as the `reasoning` field) so a future refactor
/// that touches both branches at once can't quietly regress the reasoning
/// panel on the canonical chat path.
#[test]
fn history_js_behaviour() {
    run_node_test("history.test.mjs");
}
