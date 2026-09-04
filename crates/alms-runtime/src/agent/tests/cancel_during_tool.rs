// SPDX-License-Identifier: Apache-2.0

//! #846 — cancellation while a tool is executing must synthesise the matching `tool_end`.

use super::base_runtime;
use crate::agent::*;
use crate::events::RuntimeEvent;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use crate::tools::ToolRegistry;
use alms_core::AlmsError;
use alms_session::{SessionConfig, SessionManager};

// =====================================================================
// #846 — cancel-during-tool-execution must emit a synthetic ToolEnd.
//
// Sibling of #816 (cancel during approval-wait, fixed in #845). Both
// cancel arms in `run_tool_calls` (Guarded sequential, line ~603, and
// FullControl/Autonomous parallel, line ~637) race against the inner
// `execute_tool_call` future. When the cancel arm wins, the inner future
// is dropped at an `await` point — `tool_start` was already emitted but
// `tool_end` was not. The runtime must synthesise a matching `ToolEnd`
// before unwinding so consumers (UI, audit log, persisted state) see
// the 1:1 invariant honoured. The frontend defensive sweep that
// previously masked this bug (`use-session-stream.js:1018-1023`, added
// in #594) is removed in the same PR — this test stands alone.
// =====================================================================

/// Test helper: a tool whose `execute()` awaits on a oneshot receiver
/// before returning, letting the test deterministically hold the tool
/// in-flight until it deliberately fires the cancel token.
///
/// Marked `is_auto_approved = true` so it bypasses the Guarded approval
/// gate and the inner future immediately reaches `tools.execute().await`
/// (the cancel-during-tool-execution race window).
#[derive(Debug)]
struct BlockingTestTool {
    name: String,
    /// Drained on each call. Tests pre-load this with one or more
    /// receivers; each `execute()` invocation pops one and awaits it.
    /// Once the channel sender is dropped (without sending), `await`
    /// returns Err and the tool returns an error.
    rx_queue: tokio::sync::Mutex<std::collections::VecDeque<tokio::sync::oneshot::Receiver<()>>>,
}

impl BlockingTestTool {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            rx_queue: tokio::sync::Mutex::new(std::collections::VecDeque::new()),
        }
    }

    async fn enqueue(&self, rx: tokio::sync::oneshot::Receiver<()>) {
        self.rx_queue.lock().await.push_back(rx);
    }
}

#[async_trait::async_trait]
impl alms_sandbox::Tool for BlockingTestTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "test-only tool that blocks on a oneshot receiver"
    }
    fn is_auto_approved(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        _params: serde_json::Value,
    ) -> alms_sandbox::SandboxResult<serde_json::Value> {
        let rx = {
            let mut q = self.rx_queue.lock().await;
            q.pop_front()
        };
        if let Some(rx) = rx {
            // Block until the test fires the sender, OR until the future
            // is dropped (cancel-during-tool-execution race). Drop is the
            // expected path for #846 tests.
            let _ = rx.await;
        }
        Ok(serde_json::json!({"ok": true}))
    }
}

fn make_runtime_for_cancel_test(
    posture: Posture,
    tool: std::sync::Arc<dyn alms_sandbox::Tool>,
    cancel_token: tokio_util::sync::CancellationToken,
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeEvent>,
) -> AgentRuntime {
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let tools = ToolRegistry::new();
    tools.register(tool);
    AgentRuntime {
        config: AgentConfig {
            posture,
            ..AgentConfig::default()
        },
        tools,
        event_sender: Some(tx),
        cancel_token: Some(cancel_token),
        ..base_runtime(LlmClient::new(llm_config).unwrap())
    }
}

/// Spawn a watcher task that consumes events from `rx`, mirrors
/// `(invocation_id, ok, result)` of every ToolEnd into `ends`, marks
/// every ToolStart's `invocation_id` in `starts`, and once the
/// observed `ToolStart` count reaches `cancel_after_n_starts` fires
/// `cancel_token`. This deterministically arranges for the cancel to
/// land after every expected inner future has registered with the
/// in-flight tracker and is parked at its blocking await — removing
/// the dependency on tokio scheduling order that an "always cancel on
/// first start" variant would have in the multi-tool parallel case
/// (Tim's nit on #846).
// Test helper return type is intentionally a tuple of two collections
// — splitting into a named alias would obscure the call sites without
// any reuse benefit (only one caller shape).
#[allow(clippy::type_complexity)]
fn spawn_cancel_on_tool_start(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<RuntimeEvent>,
    cancel_token: tokio_util::sync::CancellationToken,
    cancel_after_n_starts: usize,
) -> tokio::task::JoinHandle<(
    std::collections::HashSet<uuid::Uuid>,
    Vec<(uuid::Uuid, bool, serde_json::Value)>,
)> {
    assert!(
        cancel_after_n_starts >= 1,
        "cancel_after_n_starts must be at least 1"
    );
    tokio::spawn(async move {
        let mut starts = std::collections::HashSet::new();
        let mut ends = Vec::new();
        let mut cancelled = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                RuntimeEvent::ToolStart { invocation_id, .. } => {
                    starts.insert(invocation_id);
                    // Cancel only once we've observed the expected
                    // number of ToolStarts. For the parallel test (n=3)
                    // this guarantees all 3 inner futures have
                    // registered with the in-flight tracker before the
                    // cancel arm fires, regardless of how the runtime
                    // interleaves the watcher task with `join_all`'s
                    // synchronous walk.
                    if !cancelled && starts.len() >= cancel_after_n_starts {
                        cancel_token.cancel();
                        cancelled = true;
                    }
                }
                RuntimeEvent::ToolEnd {
                    invocation_id,
                    ok,
                    result,
                    ..
                } => {
                    ends.push((invocation_id, ok, result));
                }
                _ => {}
            }
        }
        (starts, ends)
    })
}

/// #846 — Guarded sequential cancel arm: a non-conflicting tool starts
/// executing under Guarded posture (auto-approved → no approval gate),
/// the test cancels mid-execution, and `run_tool_calls` must synthesise
/// a matching `ToolEnd` for the in-flight tool before returning
/// `Cancelled`.
#[tokio::test]
async fn test_cancel_during_tool_execution_emits_tool_end_guarded() {
    use tokio_util::sync::CancellationToken;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let cancel_token = CancellationToken::new();

    let blocking = std::sync::Arc::new(BlockingTestTool::new("block_test"));
    // Pre-load one receiver — the matching sender is held by the test
    // and never fired, so the tool will block until its future is
    // dropped by the cancel arm.
    let (_tx_release, rx_release) = tokio::sync::oneshot::channel::<()>();
    blocking.enqueue(rx_release).await;

    let runtime = make_runtime_for_cancel_test(
        Posture::Guarded,
        blocking.clone() as std::sync::Arc<dyn alms_sandbox::Tool>,
        cancel_token.clone(),
        tx,
    );

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    let tool_calls = vec![ToolCall::new("tc1", "block_test", "{}")];
    let invocation_id = uuid::Uuid::new_v4();
    let invocation_ids = vec![invocation_id];

    // Guarded sequential cancel arm — single tool, cancel after the
    // first (only) ToolStart.
    let watcher = spawn_cancel_on_tool_start(rx, cancel_token.clone(), 1);

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &invocation_ids,
            &[],
            &session_manager,
            session.id,
            false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;

    // Drop runtime so the event channel closes and the watcher task's
    // `recv` loop terminates.
    drop(runtime);
    let (starts, ends) = watcher.await.unwrap();

    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected AlmsError::Cancelled, got {:?}",
        result
    );
    assert!(
        starts.contains(&invocation_id),
        "tool_start must have been emitted before cancel landed (#846)"
    );

    let our_ends: Vec<_> = ends
        .iter()
        .filter(|(id, _, _)| *id == invocation_id)
        .collect();
    assert_eq!(
        our_ends.len(),
        1,
        "exactly one ToolEnd must be emitted for invocation {} — got {:?}",
        invocation_id,
        ends
    );
    let (_id, ok, result_val) = our_ends[0];
    assert!(!ok, "synthetic tool_end after cancel must report ok=false");
    let err_str = result_val
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        err_str.contains("cancel"),
        "synthetic tool_end result.error should mention cancellation, got {:?}",
        result_val
    );
}

/// #846 — FullControl/Autonomous parallel cancel arm: 3 tools run
/// concurrently in `join_all` under FullControl, the test cancels
/// mid-execution, and `run_tool_calls` must synthesise a matching
/// `ToolEnd` for *each* in-flight tool before returning `Cancelled`.
/// This exercises the harder of the two arms — multiple in-flight calls
/// at cancel time, each needs its own synthetic event.
#[tokio::test]
async fn test_cancel_during_tool_execution_emits_tool_end_parallel() {
    use tokio_util::sync::CancellationToken;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let cancel_token = CancellationToken::new();

    let blocking = std::sync::Arc::new(BlockingTestTool::new("block_test"));
    // Three calls — pre-load three receivers, hold all three senders
    // unfired so all three futures park at their blocking awaits.
    let mut held_senders = Vec::new();
    for _ in 0..3 {
        let (s, r) = tokio::sync::oneshot::channel::<()>();
        blocking.enqueue(r).await;
        held_senders.push(s);
    }

    let runtime = make_runtime_for_cancel_test(
        Posture::FullControl,
        blocking.clone() as std::sync::Arc<dyn alms_sandbox::Tool>,
        cancel_token.clone(),
        tx,
    );

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    let tool_calls = vec![
        ToolCall::new("tc1", "block_test", "{}"),
        ToolCall::new("tc2", "block_test", "{}"),
        ToolCall::new("tc3", "block_test", "{}"),
    ];
    let inv_ids: Vec<uuid::Uuid> = (0..3).map(|_| uuid::Uuid::new_v4()).collect();

    // Cancel only after observing all 3 ToolStarts. This removes the
    // dependency on tokio scheduling order — instead of trusting that
    // `join_all`'s synchronous walk polls all 3 inner futures through
    // their first await before the watcher task is scheduled, we wait
    // until the watcher has actually seen 3 ToolStart events, which
    // proves all 3 invocations are registered in the in-flight tracker
    // before the cancel arm fires (Tim's nit on #846).
    let watcher = spawn_cancel_on_tool_start(rx, cancel_token.clone(), 3);

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &inv_ids,
            &[],
            &session_manager,
            session.id,
            false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;

    drop(runtime);
    drop(held_senders);
    let (starts, ends) = watcher.await.unwrap();

    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected AlmsError::Cancelled, got {:?}",
        result
    );

    for inv in &inv_ids {
        assert!(
            starts.contains(inv),
            "tool_start missing for invocation {} — test setup broken",
            inv
        );
        let our_ends: Vec<_> = ends.iter().filter(|(id, _, _)| id == inv).collect();
        assert_eq!(
            our_ends.len(),
            1,
            "expected exactly one ToolEnd for invocation {} — got {:?} \
             (the parallel cancel arm must synthesise one ToolEnd per \
             in-flight tool, no more no less; #846)",
            inv,
            ends
        );
        let (_id, ok, result_val) = our_ends[0];
        assert!(
            !ok,
            "synthetic tool_end after cancel must report ok=false (inv {})",
            inv
        );
        let err_str = result_val
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            err_str.contains("cancel"),
            "synthetic tool_end result.error should mention cancellation \
             (inv {}), got {:?}",
            inv,
            result_val
        );
    }
}

/// #1078 — A mixed parallel batch where tool A completes before the
/// cancel arrives and tools B / C are still in-flight at cancel time.
/// The fix in #1078 says: A's `Tool`-role row MUST be persisted to
/// `session_messages`, AND a matching record MUST land in
/// `tool_call_records`, before `Err(Cancelled)` propagates. Pre-fix,
/// A's result was buffered inside `join_all`'s internal vec and dropped
/// when the cancel arm won — the next run's rebuild would synthesise an
/// `INTERRUPTED_TOOL_RESULT` for tool A even though it had succeeded.
///
/// Sequencing is event-driven (Tim's nit on #846, same pattern as the
/// no-double-emission test below): the watcher fires
/// `cancel_token.cancel()` the moment it observes A's `ToolEnd`, which
/// proves A reached its sync terminal section (unregister + audit +
/// emit) BEFORE the cancel could possibly land. Under the single-
/// threaded `#[tokio::test]` executor, that also guarantees the drain
/// loop has written `completed_results[A] = Some(...)` before the
/// select! cancel arm wins, because the drain loop runs
/// cooperatively without yielding between "fu.next() returned A's
/// result" and "next fu.next().await parks on B / C".
#[tokio::test]
async fn test_cancel_persists_completed_parallel_results_1078() {
    use tokio_util::sync::CancellationToken;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let cancel_token = CancellationToken::new();

    let blocking = std::sync::Arc::new(BlockingTestTool::new("block_test"));
    // Three calls. We pre-fire A's release sender so its `rx.await`
    // inside `execute()` resolves immediately when polled. B and C's
    // senders are held by the test and never fired — those two futures
    // will stay parked at their blocking awaits forever, so the
    // FuturesUnordered drain loop yields control after collecting A's
    // result, and the cancel arm of the outer `select!` can win.
    let (release_a_tx, release_a_rx) = tokio::sync::oneshot::channel::<()>();
    let _ = release_a_tx.send(());
    blocking.enqueue(release_a_rx).await;

    let (held_b_tx, release_b_rx) = tokio::sync::oneshot::channel::<()>();
    blocking.enqueue(release_b_rx).await;
    let (held_c_tx, release_c_rx) = tokio::sync::oneshot::channel::<()>();
    blocking.enqueue(release_c_rx).await;

    let runtime = make_runtime_for_cancel_test(
        Posture::FullControl,
        blocking.clone() as std::sync::Arc<dyn alms_sandbox::Tool>,
        cancel_token.clone(),
        tx,
    );

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    // Three distinct tool_call_ids so we can identify A's persisted
    // `ToolResult` row in the session history afterwards. The first
    // tool call ("tc-A") is the one whose release sender we pre-fired.
    let tool_calls = vec![
        ToolCall::new("tc-A", "block_test", "{}"),
        ToolCall::new("tc-B", "block_test", "{}"),
        ToolCall::new("tc-C", "block_test", "{}"),
    ];
    let inv_ids: Vec<uuid::Uuid> = (0..3).map(|_| uuid::Uuid::new_v4()).collect();

    // Watcher: fires `cancel_token.cancel()` on the first observed
    // `ToolEnd` (which is tool A's success, because B and C never
    // complete). Mirrors the event-driven sequencing pattern from
    // `test_no_double_tool_end_when_tool_ok_then_cancel`.
    let watcher = {
        let cancel_token = cancel_token.clone();
        tokio::spawn(async move {
            let mut rx = rx;
            let mut starts = std::collections::HashSet::new();
            let mut ends: Vec<(uuid::Uuid, bool, serde_json::Value)> = Vec::new();
            let mut cancelled = false;
            while let Some(ev) = rx.recv().await {
                match ev {
                    RuntimeEvent::ToolStart { invocation_id, .. } => {
                        starts.insert(invocation_id);
                    }
                    RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok,
                        result,
                        ..
                    } => {
                        ends.push((invocation_id, ok, result));
                        if !cancelled {
                            cancel_token.cancel();
                            cancelled = true;
                        }
                    }
                    _ => {}
                }
            }
            (starts, ends)
        })
    };

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &inv_ids,
            &[],
            &session_manager,
            session.id,
            /* is_dm */ false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;

    // Drop runtime + held senders so the event channel + the still-
    // parked B/C inner futures clean up and the watcher's loop exits.
    drop(runtime);
    drop(held_b_tx);
    drop(held_c_tx);
    let (_starts, ends) = watcher.await.unwrap();

    // 1) The outer call returns Cancelled — A's persistence happens
    //    on the cancel arm of the parallel branch in `run_tool_calls`.
    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected AlmsError::Cancelled, got {:?}",
        result
    );

    // 2) Exactly one of the three `ToolEnd` events must report ok=true
    //    — the real success event for tool A. The other two are
    //    synthetic cancel events (ok=false, `error: "run cancelled"`).
    let ok_ends: Vec<_> = ends.iter().filter(|(_, ok, _)| *ok).collect();
    assert_eq!(
        ok_ends.len(),
        1,
        "expected exactly one ok=true ToolEnd (the completed tool A) \
         and two synthetic cancel ToolEnds for B + C, got {:?}",
        ends
    );

    // 3) The session message log must contain exactly one `Role::Tool`
    //    row, and that row's `tool_id` must be tool A's id. Pre-#1078
    //    this row was missing — `process_tool_results` was bypassed by
    //    the cancel arm and A's result was silently dropped.
    let history = session_manager.get_history(session.id).unwrap();
    let tool_rows: Vec<&alms_session::Message> = history
        .iter()
        .filter(|m| matches!(m.role, alms_session::Role::Tool))
        .collect();
    assert_eq!(
        tool_rows.len(),
        1,
        "exactly one Tool-role row must be persisted after cancel \
         (tool A — the one that completed before the cancel landed); \
         tools B and C must NOT be persisted because they were still \
         in-flight when the cancel arrived. got {} rows: {:?}",
        tool_rows.len(),
        tool_rows
    );

    let persisted_tool_id = match &tool_rows[0].content {
        alms_session::Content::ToolResult { tool_id, .. } => tool_id.clone(),
        other => panic!("unexpected Tool-role content shape: {:?}", other),
    };
    assert_eq!(
        persisted_tool_id, "tc-A",
        "the persisted tool result must be tool A's (the one that \
         completed), not B or C. got tool_id={}",
        persisted_tool_id
    );

    // 4) The persisted row's metadata must carry `ok: true` for tool A
    //    (it succeeded before the cancel), so the rebuild pipeline
    //    treats it as a real success and not an interrupted call.
    let meta = tool_rows[0]
        .metadata
        .as_ref()
        .expect("persisted tool result must have metadata");
    assert_eq!(
        meta.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "tool A's persisted ok flag must be true — its execute() returned \
         Ok before the cancel arm fired. metadata={:?}",
        meta
    );

    // 5) `tool_call_records` must include a `ToolCallRole::Tool` entry
    //    for tool A so the gateway's per-run records (#696) surface the
    //    completed tool to the UI / `GET /runs/{id}/tool-calls`.
    let tool_records: Vec<&alms_core::ToolCallRecord> = tool_call_records
        .iter()
        .filter(|r| matches!(r.role, alms_core::ToolCallRole::Tool))
        .collect();
    assert_eq!(
        tool_records.len(),
        1,
        "exactly one Tool-role ToolCallRecord must be appended on the \
         cancel arm (for tool A). got {} records: {:?}",
        tool_records.len(),
        tool_records
    );
    assert_eq!(
        tool_records[0].tool_id.as_deref(),
        Some("tc-A"),
        "the per-run record must reference tool A's tool_id"
    );
}

/// #1090 — Guarded sequential cancel arm: a two-tool batch where tool A
/// completes before the cancel arrives and tool B is still in-flight at
/// cancel time. Same shape as `test_cancel_persists_completed_parallel_results_1078`
/// but for the Guarded `Posture::Guarded` posture's sequential cancel
/// path. Pre-#1090, A's `Tool`-role row was silently dropped because the
/// Guarded cancel arm bypassed `process_tool_results` — the only
/// persistence site for completed tool results — and the next run's
/// rebuild would synthesise `INTERRUPTED_TOOL_RESULT` for A even though
/// it had succeeded.
///
/// Two cancel branches exist in the Guarded arm:
///
/// * **Branch 1**: inter-tool `is_cancelled()` check (cancel landed
///   between iterations). Persists prior `results` entries before
///   unwinding.
/// * **Branch 2**: outer `tokio::select!` cancel arm (cancel arrived
///   mid-tool-execution). Persists prior `results` entries, then
///   synthesises `ToolEnd`s for the in-flight subset via
///   `synthesize_cancel_tool_ends`.
///
/// Under tokio's single-threaded `#[tokio::test]` executor, this test
/// deterministically lands in Branch 2: A's tool body completes
/// synchronously (its release sender is pre-fired so `rx.await` polls
/// straight through), the runtime task pushes A's Ok to `results` and
/// proceeds to B's iteration without yielding, B's `execute_tool_call`
/// emits ToolStart and parks at B's blocking `rx.await`. The watcher
/// task is finally scheduled, sees A's ToolEnd, fires
/// `cancel_token.cancel()`. The outer select's cancel arm wins because
/// B's inner future is parked. Both branches route through the same
/// `persist_completed_guarded_results_on_cancel` helper, so a Branch 2
/// test covers the helper for Branch 1 too — the only difference
/// between branches is which `return Err(Cancelled)` site is reached,
/// and the persistence pass that precedes them is byte-identical.
#[tokio::test]
async fn test_cancel_persists_completed_guarded_results_1090() {
    use tokio_util::sync::CancellationToken;

    // Bring the trait into scope so `is_auto_approved()` resolves below.
    use alms_sandbox::Tool as _;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let cancel_token = CancellationToken::new();

    let blocking = std::sync::Arc::new(BlockingTestTool::new("block_test"));
    // Pin the auto-approval invariant: this test deterministically
    // covers Branch 2 (cancel-during-tool-execution inside the inner
    // `select!`) only if `BlockingTestTool` bypasses the Guarded
    // approval gate. If a future maintainer flips
    // `is_auto_approved()` to `false` (or adds a manual-approval
    // variant) the runtime would instead park on the approval queue
    // and the cancel arm in `execute_tool_call`'s approval-wait
    // branch would fire — a different code path that does not run
    // `persist_completed_guarded_results_on_cancel`. Failing fast
    // here is preferable to a green test that exercises the wrong
    // branch. (Tim's nit on #1090.)
    debug_assert!(
        blocking.is_auto_approved(),
        "BlockingTestTool must be auto-approved for this test to land in \
         Branch 2 of `run_tool_calls`'s Guarded arm; otherwise the runtime \
         parks on the approval queue and the cancel persistence path under \
         test is bypassed."
    );

    // Two calls — A's release sender is pre-fired so A's `rx.await`
    // inside `execute()` resolves immediately. B's release sender is
    // held by the test and never fired, so B's `execute()` parks at its
    // blocking await forever and the FuturesUnordered drain loop yields
    // control after collecting A's result. Same pattern as the parallel
    // test, scaled down to two tools because the Guarded arm runs
    // sequentially — A must complete before B's iteration even starts.
    let (release_a_tx, release_a_rx) = tokio::sync::oneshot::channel::<()>();
    let _ = release_a_tx.send(());
    blocking.enqueue(release_a_rx).await;

    let (held_b_tx, release_b_rx) = tokio::sync::oneshot::channel::<()>();
    blocking.enqueue(release_b_rx).await;

    let runtime = make_runtime_for_cancel_test(
        Posture::Guarded,
        blocking.clone() as std::sync::Arc<dyn alms_sandbox::Tool>,
        cancel_token.clone(),
        tx,
    );

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    // Two distinct tool_call_ids so we can identify A's persisted
    // `ToolResult` row in the session history afterwards.
    let tool_calls = vec![
        ToolCall::new("tc-A", "block_test", "{}"),
        ToolCall::new("tc-B", "block_test", "{}"),
    ];
    let inv_ids: Vec<uuid::Uuid> = (0..2).map(|_| uuid::Uuid::new_v4()).collect();

    // Watcher: fires `cancel_token.cancel()` on the first observed
    // `ToolEnd` (which is tool A's success — B never completes). Mirrors
    // the event-driven sequencing pattern from
    // `test_cancel_persists_completed_parallel_results_1078`.
    let watcher = {
        let cancel_token = cancel_token.clone();
        tokio::spawn(async move {
            let mut rx = rx;
            let mut starts = std::collections::HashSet::new();
            let mut ends: Vec<(uuid::Uuid, bool, serde_json::Value)> = Vec::new();
            let mut cancelled = false;
            while let Some(ev) = rx.recv().await {
                match ev {
                    RuntimeEvent::ToolStart { invocation_id, .. } => {
                        starts.insert(invocation_id);
                    }
                    RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok,
                        result,
                        ..
                    } => {
                        ends.push((invocation_id, ok, result));
                        if !cancelled {
                            cancel_token.cancel();
                            cancelled = true;
                        }
                    }
                    _ => {}
                }
            }
            (starts, ends)
        })
    };

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &inv_ids,
            &[],
            &session_manager,
            session.id,
            /* is_dm */ false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;

    // Drop runtime + held sender so the event channel + the still-
    // parked B inner future clean up and the watcher's loop exits.
    drop(runtime);
    drop(held_b_tx);
    let (_starts, ends) = watcher.await.unwrap();

    // 1) The outer call returns Cancelled — A's persistence happens on
    //    the cancel arm of the Guarded branch in `run_tool_calls`.
    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected AlmsError::Cancelled, got {:?}",
        result
    );

    // 2) Exactly one of the two `ToolEnd` events must report ok=true
    //    — the real success event for tool A. The other is the synthetic
    //    cancel event for B (ok=false, `error: "run cancelled"`).
    let ok_ends: Vec<_> = ends.iter().filter(|(_, ok, _)| *ok).collect();
    assert_eq!(
        ok_ends.len(),
        1,
        "expected exactly one ok=true ToolEnd (the completed tool A) \
         and one synthetic cancel ToolEnd for B, got {:?}",
        ends
    );

    // 3) The session message log must contain exactly one `Role::Tool`
    //    row, and that row's `tool_id` must be tool A's id. Pre-#1090
    //    this row was missing — `process_tool_results` was bypassed by
    //    the Guarded cancel arm and A's result was silently dropped.
    let history = session_manager.get_history(session.id).unwrap();
    let tool_rows: Vec<&alms_session::Message> = history
        .iter()
        .filter(|m| matches!(m.role, alms_session::Role::Tool))
        .collect();
    assert_eq!(
        tool_rows.len(),
        1,
        "exactly one Tool-role row must be persisted after Guarded cancel \
         (tool A — the one that completed before the cancel landed); \
         tool B must NOT be persisted because it was still in-flight \
         when the cancel arrived. got {} rows: {:?}",
        tool_rows.len(),
        tool_rows
    );

    let persisted_tool_id = match &tool_rows[0].content {
        alms_session::Content::ToolResult { tool_id, .. } => tool_id.clone(),
        other => panic!("unexpected Tool-role content shape: {:?}", other),
    };
    assert_eq!(
        persisted_tool_id, "tc-A",
        "the persisted tool result must be tool A's (the one that \
         completed), not B. got tool_id={}",
        persisted_tool_id
    );

    // 4) The persisted row's metadata must carry `ok: true` for tool A.
    let meta = tool_rows[0]
        .metadata
        .as_ref()
        .expect("persisted tool result must have metadata");
    assert_eq!(
        meta.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "tool A's persisted ok flag must be true — its execute() returned \
         Ok before the cancel arm fired. metadata={:?}",
        meta
    );

    // 5) `tool_call_records` must include a `ToolCallRole::Tool` entry
    //    for tool A so the gateway's per-run records surface the
    //    completed tool to the UI / `GET /runs/{id}/tool-calls`.
    let tool_records: Vec<&alms_core::ToolCallRecord> = tool_call_records
        .iter()
        .filter(|r| matches!(r.role, alms_core::ToolCallRole::Tool))
        .collect();
    assert_eq!(
        tool_records.len(),
        1,
        "exactly one Tool-role ToolCallRecord must be appended on the \
         Guarded cancel arm (for tool A). got {} records: {:?}",
        tool_records.len(),
        tool_records
    );
    assert_eq!(
        tool_records[0].tool_id.as_deref(),
        Some("tc-A"),
        "the per-run record must reference tool A's tool_id"
    );
}

/// #846 — No-double-emission regression: a tool that finishes Ok
/// followed (in real time) by a cancel arrival must not produce two
/// `ToolEnd` events for the same invocation. The unregister-before-emit
/// protocol inside `execute_tool_call` ensures the entry is gone from
/// the in-flight tracker before the outer cancel arm could even see it.
///
/// Sequencing is event-driven, not wall-clock based (Tim's nit on
/// #846): the watcher task fires `cancel_token.cancel()` the moment it
/// observes a `ToolEnd`, which proves the success path's
/// unregister-before-emit step has already run by the time the cancel
/// could possibly land. Nothing depends on `tokio::time::sleep`.
#[tokio::test]
async fn test_no_double_tool_end_when_tool_ok_then_cancel() {
    use tokio_util::sync::CancellationToken;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let cancel_token = CancellationToken::new();

    let blocking = std::sync::Arc::new(BlockingTestTool::new("block_test"));
    // Single call with a sender that we WILL fire before cancelling —
    // forces the inner branch of `select!` to win and emit the normal
    // success ToolEnd. The cancel arrives after, by which point the
    // tracker is empty so no synthetic should be emitted.
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    blocking.enqueue(release_rx).await;

    let runtime = make_runtime_for_cancel_test(
        Posture::Guarded,
        blocking.clone() as std::sync::Arc<dyn alms_sandbox::Tool>,
        cancel_token.clone(),
        tx,
    );

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    let tool_calls = vec![ToolCall::new("tc1", "block_test", "{}")];
    let invocation_id = uuid::Uuid::new_v4();
    let invocation_ids = vec![invocation_id];

    // Watcher: counts ToolEnd events for this invocation, and on the
    // FIRST observed ToolEnd fires `cancel_token.cancel()`. Because
    // the success ToolEnd is sent only AFTER the inner future has
    // already removed itself from the in-flight tracker (the
    // unregister-before-emit protocol in `execute_tool_call`), this
    // guarantees that the cancel — if it races into the outer
    // `run_tool_calls` cancel arm at all — finds an empty tracker and
    // emits zero synthetic ToolEnds. Replaces a fragile sleep(20ms)/
    // sleep(200ms) pair with deterministic event-based sequencing.
    let watcher = {
        let cancel_token = cancel_token.clone();
        tokio::spawn(async move {
            let mut rx = rx;
            let mut tool_end_count = 0usize;
            while let Some(ev) = rx.recv().await {
                if let RuntimeEvent::ToolEnd {
                    invocation_id: id, ..
                } = ev
                    && id == invocation_id
                {
                    tool_end_count += 1;
                    if tool_end_count == 1 {
                        cancel_token.cancel();
                    }
                }
            }
            tool_end_count
        })
    };

    // Release the tool synchronously — no wall-clock wait needed. The
    // runtime has not started yet, but the inner await on the receiver
    // sees the value as soon as it polls.
    let _ = release_tx.send(());

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &invocation_ids,
            &[],
            &session_manager,
            session.id,
            false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;
    drop(runtime);

    let tool_end_count = watcher.await.unwrap();

    assert!(
        result.is_ok(),
        "tool finished normally, run_tool_calls should return Ok, got {:?}",
        result
    );
    assert_eq!(
        tool_end_count, 1,
        "exactly one ToolEnd must be emitted per invocation — even if a \
         cancel arrives after the inner future already removed itself \
         from the in-flight tracker (#846 no-double-emission protocol)"
    );
}
