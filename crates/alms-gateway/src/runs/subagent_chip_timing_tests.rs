//! Timing pins for the subagent status-bar chip's terminal signal (issue #1).
//!
//! The reported symptom was that the chip above the message composer only
//! cleared once the PARENT run — the run that called `invoke_agent` — ended,
//! rather than when the subagent's own session finished. The frontend half of
//! that bug is fixed in `static/ui/hooks/use-session-stream.js`; these tests
//! pin the wire-level contract the frontend depends on, so a backend change
//! can never reintroduce the symptom by deferring the terminal signal to the
//! parent's run end.
//!
//! Two facts are pinned, one per subagent class, both by driving a REAL
//! `execute_run` against a scripted streaming LLM whose post-`invoke_agent`
//! turn deliberately stalls for [`PARENT_TAIL_MS`]:
//!
//! 1. FOREGROUND (`background` absent/false): the parent's `tool_end` for
//!    `invoke_agent` — the chip's ONLY terminal route, because
//!    `run_subagent` fires the `SubagentCompletion` channel behind
//!    `handle.is_background` and therefore emits NO `subagent_completed` for
//!    a foreground subagent — must reach session subscribers as soon as the
//!    subagent finishes, with the parent's remaining work still ahead of it.
//!
//! 2. BACKGROUND (`background: true`): `tool_end` returns a `task_id`
//!    immediately (the frontend deliberately ignores it), and the chip's
//!    terminal route is the `subagent_completed` event off the completion
//!    channel. That event must likewise land while the parent run is still
//!    working.
//!
//! Both assertions are on the arrival ORDER and the observed GAP relative to
//! the parent's terminal event, not merely on the event existing — "the chip
//! eventually clears" is exactly the property the reported bug already had.

use crate::gateway::GatewayConfig;
use crate::server::AppState;
use alms_core::{AgentId, Run};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// How long the scripted LLM stalls on the parent's post-tool turn. The
/// parent run cannot finish before this elapses, so any terminal chip signal
/// observed earlier is provably independent of the parent's remaining work.
const PARENT_TAIL_MS: u64 = 1_000;

/// Minimum gap (ms) required between the chip's terminal signal and the
/// parent's `run_finished`. Comfortably under `PARENT_TAIL_MS` so the pin is
/// about the ordering, not about scheduler jitter.
const MIN_LEAD_MS: u128 = 500;

/// Read one complete HTTP request (headers + `Content-Length` body) off the
/// socket. Mirrors the helper the `alms-runtime` scripted-LLM tests use.
async fn read_full_http_request(sock: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(header_end) = text.find("\r\n\r\n") {
            let content_length = text[..header_end]
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buf.len() >= header_end + 4 + content_length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// One OpenAI-style streamed text turn (the default `openrouter` provider
/// parses this wire shape).
fn text_turn(text: &str) -> String {
    format!(
        concat!(
            "data: {{\"id\":\"txt\",\"object\":\"chat.completion.chunk\",\"created\":2,",
            "\"model\":\"test-model\",\"choices\":[{{\"index\":0,",
            "\"delta\":{{\"role\":\"assistant\",\"content\":\"{}\"}},",
            "\"finish_reason\":\"stop\"}}]}}\n\n",
            "data: [DONE]\n\n"
        ),
        text
    )
}

/// One OpenAI-style streamed `invoke_agent` tool call.
fn invoke_agent_turn(background: bool) -> String {
    let args = if background {
        "{\\\"task\\\":\\\"research\\\",\\\"background\\\":true}"
    } else {
        "{\\\"task\\\":\\\"research\\\"}"
    };
    format!(
        concat!(
            "data: {{\"id\":\"tc\",\"object\":\"chat.completion.chunk\",\"created\":1,",
            "\"model\":\"test-model\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",",
            "\"tool_calls\":[{{\"index\":0,\"id\":\"{}\",\"type\":\"function\",",
            "\"function\":{{\"name\":\"invoke_agent\",\"arguments\":\"{}\"}}}}]}},",
            "\"finish_reason\":\"tool_calls\"}}]}}\n\n",
            "data: [DONE]\n\n"
        ),
        PARENT_TOOL_CALL_ID, args
    )
}

/// Provider-assigned tool-call id for the parent's `invoke_agent` call. Also
/// the discriminator the stub uses to recognise the parent's FOLLOW-UP turn
/// (only that request carries the tool result referencing this id).
const PARENT_TOOL_CALL_ID: &str = "call_parent_invoke";

/// The parent's first user input. Present only in the parent's requests.
const PARENT_INPUT: &str = "delegate the research please";

/// One observed SSE event: its type, payload, and arrival time (ms since the
/// run was spawned).
struct Observed {
    event_type: String,
    data: serde_json::Value,
    at_ms: u128,
}

/// Drive a real parent run that calls `invoke_agent` (foreground or
/// background) and return every SSE event observed on the parent's session
/// stream, timestamped.
///
/// The scripted LLM routes by request CONTENT rather than call order, because
/// a background subagent's turn runs concurrently with the parent's follow-up
/// turn.
async fn observe_parent_session_stream(background: bool) -> Vec<Observed> {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let tool_call_turn = invoke_agent_turn(background);
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let tool_call_turn = tool_call_turn.clone();
            tokio::spawn(async move {
                let req = read_full_http_request(&mut sock).await;
                let body = if req.contains(PARENT_TOOL_CALL_ID) {
                    // The parent's follow-up turn: it carries the tool result
                    // for the invoke_agent call. Stall so the parent still has
                    // work left long after the subagent is done.
                    tokio::time::sleep(Duration::from_millis(PARENT_TAIL_MS)).await;
                    text_turn("parent finished")
                } else if req.contains(PARENT_INPUT) {
                    tool_call_turn
                } else {
                    // The subagent's own turn — finishes immediately.
                    text_turn("subagent finished")
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });

    let llm_config = alms_runtime::LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 30,
        stream_chunk_timeout_secs: 30,
        ..alms_runtime::LlmConfig::default()
    };
    let gateway_config = GatewayConfig {
        db_path: Some(":memory:".to_string()),
        llm_config,
        ..GatewayConfig::default()
    };
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let (trigger_tx, _trigger_rx) = mpsc::channel(64);
    let (dm_event_tx, _dm_event_rx) = mpsc::channel(64);
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();
    // Autonomous so `invoke_agent` (not auto-approved) executes without an
    // approval round-trip; the event ordering under test is posture-agnostic.
    state.agent_config.write().posture = alms_runtime::Posture::Autonomous;

    // The production loop that converts a `SubagentCompletion` into the
    // `subagent_completed` SSE event — the background chip's terminal route.
    tokio::spawn(super::notifications::completion_notification_loop(
        completion_rx,
        state.clone(),
    ));

    let agent_id = AgentId::new();
    let session_id = state.session_manager.get_or_create(agent_id, "web").id;
    // Subscribe BEFORE the run so every assertion is about LIVE delivery to a
    // session subscriber (what the browser's EventSource sees), not a replay
    // of the persisted event log.
    let mut rx = state.run_manager.subscribe_session(session_id);

    let run = Run::new(session_id, agent_id, PARENT_INPUT.to_string());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let run_state = state.clone();
    let started = Instant::now();
    let run_handle = tokio::spawn(async move {
        super::lifecycle::execute_run(
            run_state,
            super::RunParams {
                run_id,
                session_id,
                agent_id,
                input: run.input,
                context_id: "web".to_string(),
                cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
                dm_ended_peer: None,
            },
        )
        .await;
    });

    // Collect until the PARENT's run_finished, then drain briefly so any
    // trailing event (e.g. the notification run a background completion
    // enqueues) is visible to the assertions.
    let mut observed = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut parent_finished = false;
    loop {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(event)) => {
                let is_parent_terminal = matches!(
                    event.event_type.as_str(),
                    "run_finished" | "run_error" | "run_cancelled"
                ) && event.data.get("run_id").and_then(|v| v.as_str())
                    == Some(run_id.0.to_string().as_str());
                observed.push(Observed {
                    event_type: event.event_type,
                    data: event.data,
                    at_ms: started.elapsed().as_millis(),
                });
                if is_parent_terminal {
                    parent_finished = true;
                }
            }
            Ok(None) => break,
            Err(_) => {
                if parent_finished || Instant::now() > deadline {
                    break;
                }
            }
        }
    }
    run_handle.await.unwrap();
    shutdown_token.cancel();
    observed
}

/// Arrival time of the first event of `event_type`.
fn first_at(observed: &[Observed], event_type: &str) -> Option<u128> {
    observed
        .iter()
        .find(|e| e.event_type == event_type)
        .map(|e| e.at_ms)
}

fn summarize(observed: &[Observed]) -> Vec<(String, u128)> {
    observed
        .iter()
        .map(|e| (e.event_type.clone(), e.at_ms))
        .collect()
}

/// FOREGROUND subagent: the chip's only terminal route is `tool_end`, and it
/// must arrive as soon as the subagent finishes — with the whole
/// [`PARENT_TAIL_MS`] of parent work still ahead of it.
///
/// The pin is deliberately a RELATIONSHIP, not "a terminal event eventually
/// arrives": the reported bug already satisfied the latter.
#[tokio::test]
async fn foreground_subagent_tool_end_leads_the_parent_run_terminal() {
    let observed = observe_parent_session_stream(false).await;

    let tool_end_at = first_at(&observed, "tool_end").unwrap_or_else(|| {
        panic!(
            "a foreground invoke_agent must produce a tool_end on the parent \
             session stream; got: {:?}",
            summarize(&observed)
        )
    });
    let finished_at = first_at(&observed, "run_finished").unwrap_or_else(|| {
        panic!(
            "the parent run must finish; got: {:?}",
            summarize(&observed)
        )
    });

    assert!(
        finished_at.saturating_sub(tool_end_at) >= MIN_LEAD_MS,
        "tool_end for invoke_agent must reach session subscribers while the \
         parent still has work left — it is the ONLY route a foreground \
         subagent chip has to a terminal status. Deferring it to the parent's \
         run end is issue #1's symptom. tool_end at {tool_end_at}ms, \
         run_finished at {finished_at}ms; timeline: {:?}",
        summarize(&observed)
    );

    let tool_end = observed
        .iter()
        .find(|e| e.event_type == "tool_end")
        .expect("checked above");
    assert_eq!(
        tool_end.data.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "the foreground tool_end must report success"
    );
    assert!(
        tool_end.data["result"].get("task_id").is_none(),
        "a FOREGROUND invoke_agent result must carry no `task_id` — the \
         frontend keys its background skip on that field, and a stray one \
         would make it wait for a `subagent_completed` that never comes: {:?}",
        tool_end.data["result"]
    );
    assert!(
        tool_end.data["result"].get("session_id").is_some(),
        "the foreground tool_end result must carry the subagent session id so \
         the chip keeps its drill-down link when it goes terminal: {:?}",
        tool_end.data["result"]
    );

    // Pins the asymmetry the fix depends on: there is no second route.
    assert!(
        !observed
            .iter()
            .any(|e| e.event_type == "subagent_completed"),
        "a foreground subagent emits no `subagent_completed` (the completion \
         channel is fired behind `handle.is_background`). If that ever \
         changes, the frontend's foreground/background split in the `tool_end` \
         handler must be revisited; timeline: {:?}",
        summarize(&observed)
    );
}

/// BACKGROUND subagent: `tool_end` returns immediately with a `task_id` (the
/// frontend ignores it), so the chip's terminal route is `subagent_completed`
/// — which must likewise land while the parent run is still working.
#[tokio::test]
async fn background_subagent_completed_leads_the_parent_run_terminal() {
    let observed = observe_parent_session_stream(true).await;

    let tool_end = observed
        .iter()
        .find(|e| e.event_type == "tool_end")
        .unwrap_or_else(|| {
            panic!(
                "a background invoke_agent must still produce a tool_end; \
                 got: {:?}",
                summarize(&observed)
            )
        });
    assert!(
        tool_end.data["result"].get("task_id").is_some(),
        "the background tool_end result must carry a `task_id` — that field is \
         what tells the frontend to keep the chip running and wait for \
         `subagent_completed`: {:?}",
        tool_end.data["result"]
    );

    let completed_at = first_at(&observed, "subagent_completed").unwrap_or_else(|| {
        panic!(
            "a background subagent must produce a subagent_completed on the \
             parent session stream — it is that chip's only terminal route; \
             got: {:?}",
            summarize(&observed)
        )
    });
    let finished_at = first_at(&observed, "run_finished").unwrap_or_else(|| {
        panic!(
            "the parent run must finish; got: {:?}",
            summarize(&observed)
        )
    });

    assert!(
        finished_at.saturating_sub(completed_at) >= MIN_LEAD_MS,
        "subagent_completed must be broadcast as soon as the background \
         subagent finishes, not deferred behind the parent run (issue #1). \
         subagent_completed at {completed_at}ms, run_finished at \
         {finished_at}ms; timeline: {:?}",
        summarize(&observed)
    );

    let completed = observed
        .iter()
        .find(|e| e.event_type == "subagent_completed")
        .expect("checked above");
    assert!(
        completed.data.get("tool_invocation_id").is_some(),
        "subagent_completed must carry the parent's invoke_agent \
         tool_invocation_id so the chip resolves identity-exactly: {:?}",
        completed.data
    );
}
