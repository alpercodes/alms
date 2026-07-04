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

/// Pinned regression for issue #980: the sidebar groups sessions under
/// per-agent collapsible accordion headers. At most one agent group is
/// expanded at a time; clicking another agent header collapses the
/// previous and expands the new one. Clicking the expanded agent
/// header is a true toggle (collapses the body) without changing the
/// active agent. The default-expanded agent at boot is the
/// currently-active one.
///
/// The runtime side (Preact rendering, signal wiring, `switchAgent`
/// side-effect) is integration territory we can't easily unit-test
/// under Node — what's pinned here is the pure-function shape of the
/// expand/collapse decision in `static/ui/utils/sidebar-grouping.js`:
///   - `expandAgent(state, clickedId)` — at-most-one-at-a-time, with
///     same-agent-click pinned to "collapse" (true toggle)
///   - `defaultExpandedAgent(activeId)` — boot-time default rule
///   - `isAgentExpanded(state, agentId)` — render-time predicate
///   - `groupSessionsByAgent(sessions)` — flat-list to per-agent Map
///     with input-order preservation
#[test]
fn sidebar_grouping_js_behaviour() {
    run_node_test("sidebar-grouping.test.mjs");
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

/// Pinned regression for issue #1041: the SubagentBar live status panel
/// disappears on page reload or session switch while a subagent is still
/// in flight server-side. The fix adds
/// `rehydrateSubagentsFromHistory` in `static/ui/state/subagents.js`,
/// called from `loadSession` after `replaceMessages`, that rebuilds the
/// `activeSubagents` signal from any `invoke_agent` tool rows in the
/// freshly-loaded history that are still running (foreground:
/// `status === 'running'`; background: result has `task_id` but no
/// matching `subagent_completed` marker). The JS-side test pins:
///   - foreground in-flight: re-adds named and unnamed entries with the
///     correct synthetic key shape so `findSubagentByToolInvocationId`
///     and `trackSubagentEnd` keep matching post-reload
///   - foreground completed: skipped (already in chat history)
///   - background in-flight: re-added with `sessionId` from the parent
///     result
///   - background completed (matching marker in history): skipped
///   - live SSE-populated entries: preserved (no clobber)
///   - non-tool / non-invoke_agent messages: ignored
///   - empty / non-array input: no-op
///   - startedAt: prefers the persisted message timestamp, falls back to
///     `Date.now()` when missing
#[test]
fn subagents_rehydrate_js_behaviour() {
    run_node_test("subagents-rehydrate.test.mjs");
}

/// Behavioural coverage for `historyCoversSeal` in
/// `static/ui/utils/reasoning-coverage.js` — the load-time coverage gate that
/// decides whether a run that went terminal during a session load is added to
/// the `reasoning_delta` suppress-set (#1133 Layer 3 / Codex finding #3).
///
/// Pins the sub-race split that the gate exists to resolve:
///   - sub-race A (`historyHWM >= seal_event_id`): the messages GET captured
///     the sealed reasoning -> suppress the replayed deltas (no double-render)
///   - sub-race B (`historyHWM <  seal_event_id`): the run sealed AFTER the
///     messages GET resolved -> history lacks the reasoning -> do NOT suppress
///     so the replayed deltas render the final turn exactly once
///   - missing/null/non-numeric anchor or HWM: conservative `false`
///     (render once, never risk zero renders)
///   - string-numeric `lastEventId` compares numerically, not lexically
#[test]
fn reasoning_coverage_gate_js_behaviour() {
    run_node_test("reasoning-coverage.test.mjs");
}

/// Pinned regression for issue #1135: the Layer-3 reasoning-dedupe suppress-set
/// (`sealedReasoningRunIds`, introduced in #1133 / PR #1134) used to live only
/// on the initial `openSessionStream` `opts`, so a mid-replay EventSource
/// reconnect — which reopens with `{ lastEventId }` only — lost the set and
/// let already-sealed reasoning re-duplicate as a spurious unsealed bubble
/// until the next full reload. The fix hoists the set to a per-session
/// module-scoped store in `static/ui/state/reasoning-dedupe.js` that the
/// auto-backoff and manual reconnect paths recover after the originating
/// `opts` object is gone. The JS-side test pins:
///   - store / recover round-trip (same Set reference; null for unknown)
///   - the recover-before-clear, re-record reconnect cycle preserves the set
///   - per-session scoping (no cross-session leakage)
///   - cleanup on teardown / session switch (no unbounded growth; bounded
///     at the live session)
///   - no-op safety for falsy sessionId and non-Set values (the four
///     `openSessionStream` callers that pass no suppress-set stay inert)
///   - last-write-wins so a fresh `loadSession` supersedes the old set
#[test]
fn reasoning_dedupe_store_js_behaviour() {
    run_node_test("reasoning-dedupe.test.mjs");
}

/// Behavioural coverage for the live DM-stream render path in
/// `static/ui/hooks/use-session-stream.js`, driven through a real
/// `FakeEventSource` (source-rewrite loader — no mocking of the handlers).
///
/// Pins two arcs:
///   - #1154 B8/B9/B10 — reasoning_delta run_id bucketing, race-proof tool
///     grouping into the live `dm_reasoning` block, and positional
///     `dm_conversation_ended` banner dedupe.
///   - #1157 / #1162 — the implicit-reply live render. Under implicit DM
///     replies (#1156) the agent's final text streams as visible
///     `token_delta` AND is delivered as the `dm_message` bubble; painting it
///     into the reasoning collapsible too double-rendered it (#1157),
///     mis-attributed it to the sender before participants resolved (#1162
///     sym-1), and showed partial-then-full mid-stream (#1162 sym-2). The
///     tests pin that the reply renders exactly once (the bubble, correctly
///     attributed), the collapsible holds reasoning only (pre-tool "thinking
///     out loud" is committed at the tool boundary; the trailing reply is
///     discarded at run end), and an unresolved-participants race never
///     attributes a peer run to the sender.
///
/// Wires the file into `cargo test` / CI — previously it only ran under a
/// direct `node --test` invocation, so the DM-render regressions had no Rust
/// harness gate.
#[test]
fn dm_stream_rendering_js_behaviour() {
    run_node_test("dm-stream-rendering.test.mjs");
}

/// Behavioural coverage for the Subagent status bar in
/// `static/ui/state/subagents.js` + the `subagent_activity` / tagged-content
/// handlers in `static/ui/hooks/use-session-stream.js` and the label mapping
/// in `static/ui/utils/subagent-status.js`, driven through a real
/// `FakeEventSource` against the REAL subagents module.
///
/// Pins three arcs (#1180 follow-up, subsumes #1186):
///   - status-only display — a tagged `subagent_activity` signal sets the
///     matching entry's `activity` {kind, tool} (with key migration for
///     unnamed subagents) and `subagentStatusLabel` maps it to the concise
///     chip labels ("Reasoning…", "Using {tool}", "Writing…", …).
///   - content drop — `source_agent`-tagged `reasoning_delta` / `token_delta`
///     / `tool_start` / `tool_end` (replays from pre-status-bar event logs)
///     write NOTHING to the bar or the parent view, and a tagged `tool_end`
///     can no longer mis-close a running parent tool row. With no reasoning
///     text rendered, the #1186 buffered-fallback duplication is impossible
///     by construction.
///   - #1183 startup race — an early activity signal (before the
///     entry-creating `tool_start (invoke_agent)`) is buffered latest-wins
///     (LRU-capped, aged out, evicted on completion/clear) and applied at
///     entry creation — never creating/resurrecting a chip on its own.
#[test]
fn subagent_status_bar_js_behaviour() {
    run_node_test("subagent-status-bar.test.mjs");
}

/// Behavioural coverage for the subagent cancel-confirm flow in
/// `static/ui/state/subagent-cancel.js` — the shared decision layer behind
/// both cancel surfaces (the ✕ on RUNNING Subagent-status-bar chips and the
/// "Cancel subagent" button in the subagent session view's breadcrumb),
/// driven against the REAL module with a recording `cancelSubagent` stub.
///
/// Pins the confirm contract: arming shows the confirm and makes NO API
/// call; only an explicit Yes calls the session-keyed cancel endpoint with
/// the armed session id, exactly once (double-click safe); No / the
/// auto-revert timer / re-arming another session dismiss with NO call; the
/// unknown-session-id and terminal-status guards (`showCancelControl`,
/// nullish-id refusals) never let a cancel fire without a real session id.
///
/// Also pins the chip-lifecycle clearing (Codex P2, PR #1192), driven
/// through the REAL `state/subagents.js` wired to the same cancel-module
/// instance: the armed confirm is dismissed at the armed subagent's
/// terminal transition, at chip auto-removal, and on `clearAllSubagents`
/// (session switch) — named subagents reuse the same session id across
/// re-invocations, so a surviving confirm would pre-arm the next
/// invocation's chip and its Yes would live-fire without a confirming
/// first click. An unrelated subagent's lifecycle never dismisses it.
#[test]
fn subagent_cancel_js_behaviour() {
    run_node_test("subagent-cancel.test.mjs");
}
