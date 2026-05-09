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

/// Pinned regression for issue #858: the "Open in Explorer" button in the
/// workspace panel relies on `openWorkspaceInExplorer` from
/// `static/ui/api/workspace.js` issuing a `POST /agents/{id}/workspace/open`
/// — which in turn relies on the matching axum route in `routes.rs`. The
/// JS-side test mocks `fetch` and asserts the request URL + method + body
/// shape so a future refactor of the API client wrapper can't silently
/// drift from the registered route.
///
/// Also covers the error-code → friendly-label mapping that the click
/// handler uses on failure, against the structured-error codes that the
/// backend handler returns (`NOT_CONFIGURED`, `WORKSPACE_PATH_MISSING`,
/// `LAUNCHER_FAILED`).
#[test]
fn workspace_open_js_behaviour() {
    run_node_test("workspace-open.test.mjs");
}

/// Pinned regression for issue #873: tool-call output rendering parity.
/// `static/ui/utils/tool-output.js` mirrors the input-side renderers from
/// `tool-summary.js`/`tool-row.js`, dispatching `tool_end` payloads to a
/// per-tool structured renderer instead of dumping the raw JSON blob. The
/// JS-side test feeds representative `tool_end` payloads to the dispatcher
/// and asserts the rendered DOM contains the expected sections (status
/// pills, code-block panes, match-list rows, chat-bubble rows, etc.) and
/// does NOT contain raw-JSON-fallback shapes — so a future refactor that
/// silently breaks the structured path falls back to a visibly-different
/// view that the test catches.
#[test]
fn tool_output_js_behaviour() {
    run_node_test("tool-output.test.mjs");
}

/// Pinned regression for issues #981 (composer draft lost on session
/// switch) and #975 (queued messages lost on session switch). Both bugs
/// share an architecture — chat-component-local state with view-bound
/// lifetime — and the fix persists draft + queue per-session in
/// `localStorage` via `static/ui/state/composer-storage.js`. The JS-side
/// test pins:
///   - draft round-trip across mount/unmount and across module reload
///   - draft cleared on send (and on empty-text save)
///   - draft try-full-write-then-truncate-on-quota with console.warn
///   - queue round-trip across mount/unmount and across module reload
///   - per-session scoping (no bleed across session ids)
///   - clearComposerState() sweeps both kinds of state on session delete
///   - graceful behaviour when `localStorage` is missing or throws
///   - byte-cap branches: oversize-tail drop and single-giant-item removal
///   - load-side cap against tampered storage blobs
///   - planMountDrain pure-function contract: pins the orphan-queue fix
///     from Tim's review on PR #990 — when the run that the queue was
///     waiting on completes while the operator is on a different session
///     (or in the page-reload race window), the next mount must drain
///     the head rather than leave the queue orphaned forever.
#[test]
fn composer_storage_js_behaviour() {
    run_node_test("composer-storage.test.mjs");
}

/// Pinned regression for issue #978: the agent-create form normalizes the
/// operator's input to a slug-safe shape before POSTing to the backend so
/// "ResearchBot" / "Research Bot" / etc. never surface a 400 from
/// `validate_agent_name`. The JS-side test exercises the pure-function
/// normalizer in `static/ui/utils/agent-name.js` against every rule it
/// applies plus the empty-string boundary the caller relies on for the
/// "name required" inline error.
#[test]
fn agent_name_js_behaviour() {
    run_node_test("agent-name.test.mjs");
}

/// Pinned regression for issue #907: SSE retry-budget exhaustion used to
/// leave the user with a dead stream and only a `console.error`. The fix
/// introduces a global `streamDead` signal in
/// `static/ui/state/stream-health.js`, a click-to-reconnect path
/// (`reconnectAllStreams`), and a `window.addEventListener('online', ...)`
/// re-arm. The JS-side test pins:
///   - `streamDead` transitions on mark / clear (idempotent, only flips on
///     real empty <-> non-empty boundary)
///   - "any stream dead -> banner up" semantic: two distinct sources keep
///     the banner up until both clear
///   - `reconnectAllStreams` fan-out (both callbacks fire exactly once)
///   - last-write-wins on `register*` so HMR / test re-imports don't stack
///     handlers
///   - error in one callback doesn't block the other from firing
///   - `installOnlineReconnectListener` wires window.online -> reconnect
///     fan-out, returns a teardown that detaches cleanly
///   - the integrated mark -> click -> clear cycle the operator actually
///     sees in the banner
#[test]
fn stream_health_js_behaviour() {
    run_node_test("stream-health.test.mjs");
}

/// Pinned regression for issue #983: the agent card in the right-side
/// agents panel is now a whole-card click target (the per-card "Select"
/// button is removed). The JS-side test exercises the pure-function
/// activation predicates in `static/ui/utils/card-activation.js`:
///   - `isActivationKey(key)` — Enter / Space activate, nothing else
///   - `shouldActivateFromKey(event)` — keyboard-driven activation gate
///     with a `defaultPrevented` suppression for nested handlers
///   - `shouldActivateFromClick(event)` — click-driven activation gate
///     with the same `defaultPrevented` suppression
///
/// Pins the wired-up handler shape from `agents-tab.js`'s `AgentCard`
/// component (preventDefault on Enter / Space, ignore Tab) so a future
/// refactor that drops the keyboard activation or breaks the
/// stopPropagation defense for nested actions surfaces here.
#[test]
fn card_activation_js_behaviour() {
    run_node_test("card-activation.test.mjs");
}

/// Pinned regression for issue #986: copy-to-clipboard button on every
/// fenced code block in agent messages. The pure-function logic in
/// `static/ui/utils/code-copy.js` decides the clipboard payload —
/// extracting the inner-`<code>` text (so the language tag, which is a
/// class artifact, never leaks), stripping the single trailing newline
/// marked appends, and returning the empty sentinel on null /
/// non-element inputs. The side-effecting half (button injection,
/// `navigator.clipboard.writeText`, icon swap) lives in
/// `decorate-code-blocks.js` and is smoke-tested manually in the
/// browser.
#[test]
fn code_copy_js_behaviour() {
    run_node_test("code-copy.test.mjs");
}

/// Pinned regression for the timer-cancel-on-manual-reconnect contract
/// in `static/ui/hooks/use-agent-events.js` (#907 follow-up — Tim's
/// Suggestion 1 on PR #1001). When a manual reconnect path (banner
/// click, `online` event, or fresh `openAgentEventsStream` call) fires
/// while a backoff `setTimeout` is still pending from a previous
/// `onerror`, the pending timer must be cancelled so it doesn't
/// double-open the freshly-healthy stream a few seconds later. The
/// JS-side test stubs `EventSource` + `localStorage` and uses
/// `node:test` `mock.timers` to pin:
///   - manual reconnect mid-backoff cancels the pending reopen
///   - explicit close mid-backoff also cancels
///   - the timer *does* normally fire and reopen when nothing cancels
///     it (negative control)
///
/// The same fix shape is applied to `use-session-stream.js` but that
/// hook has a 50+ collaborator import surface that is not worth
/// standing up a parallel harness for — the cancel/clear/schedule
/// edits are byte-for-byte identical between the two hooks.
#[test]
fn agent_events_timer_js_behaviour() {
    run_node_test("agent-events-timer.test.mjs");
}
