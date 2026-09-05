// SPDX-License-Identifier: Apache-2.0

//! Agent-loop hard caps: iteration, duration and inactivity limits, the stalled-stream fallback, and the #1154 DM empty-reply nudge.

use crate::agent::*;
use crate::events::RuntimeEvent;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::AgentId;
use alms_session::{SessionConfig, SessionManager};
use alms_test_support::read_full_http_request;

// ──────────────────────────────────────────────────────────────────────
// #1154 design default #3 — loop-level DM empty-reply nudge (Tim's S2
// on PR #1156). The gate-side cases (empty reply → Errored exit etc.)
// are covered in the gateway's integration tests; this exercises the
// RUNTIME side: a scripted two-turn LLM where the first turn yields no
// deliverable text, asserting the loop (a) emits the
// `DM_EMPTY_REPLY_RETRY` warning, (b) injects the nudge message into
// the next LLM request, and (c) returns the second turn's text as a
// deliverable output.

#[tokio::test]
async fn dm_empty_reply_nudge_retries_once_then_delivers_second_turn_text() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    const REPLY_TEXT: &str = "Hello alice, here is a real reply.";

    // Scripted two-turn LLM: turn 1 streams no content (empty delta +
    // stop), turn 2 streams a real text reply. OpenAI-style SSE bodies
    // because the default provider (`openrouter`) parses the OpenAI
    // wire shape.
    let turn1_body = concat!(
        "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"test-model\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let turn2_body = format!(
        concat!(
            "data: {{\"id\":\"t2\",\"object\":\"chat.completion.chunk\",\"created\":2,",
            "\"model\":\"test-model\",\"choices\":[{{\"index\":0,",
            "\"delta\":{{\"role\":\"assistant\",\"content\":\"{}\"}},",
            "\"finish_reason\":\"stop\"}}]}}\n\n",
            "data: [DONE]\n\n"
        ),
        REPLY_TEXT
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    // Capture the request bodies so the nudge-injection assertion can
    // look at what the loop actually sent on the second turn.
    let captured: std::sync::Arc<tokio::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let captured_writer = captured.clone();
    tokio::spawn(async move {
        for body in [turn1_body.to_string(), turn2_body] {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let req = read_full_http_request(&mut sock).await;
            captured_writer.lock().await.push(req);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply()
    .with_event_sender(tx);

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    // (c) The loop must complete with the SECOND turn's text — i.e. it
    // retried after the empty first turn — and that text must be a
    // deliverable DM reply for the gateway's completion gate.
    assert!(tool_calls.is_empty(), "no tool calls were scripted");
    let output = result.expect("agent_loop must succeed after the nudge retry");
    assert_eq!(output.response, REPLY_TEXT);
    assert!(
        alms_core::deliverable_dm_reply(&output.response, output.reasoning.as_deref()).is_some(),
        "the second-turn text must classify as deliverable"
    );

    // (a) Exactly one DM_EMPTY_REPLY_RETRY warning, and no terminal
    // DM_EMPTY_REPLY (the retry succeeded).
    let mut retry_warnings = 0;
    let mut exhausted_warnings = 0;
    while let Ok(event) = rx.try_recv() {
        if let RuntimeEvent::Warning { code, .. } = event {
            match code.as_str() {
                "DM_EMPTY_REPLY_RETRY" => retry_warnings += 1,
                "DM_EMPTY_REPLY" => exhausted_warnings += 1,
                _ => {}
            }
        }
    }
    assert_eq!(
        retry_warnings, 1,
        "exactly one DM_EMPTY_REPLY_RETRY warning must be emitted"
    );
    assert_eq!(
        exhausted_warnings, 0,
        "the nudge succeeded, so no terminal DM_EMPTY_REPLY warning"
    );

    // (b) The nudge message was injected into the second LLM request.
    let requests = captured.lock().await;
    assert_eq!(requests.len(), 2, "the loop must have made two LLM calls");
    assert!(
        !requests[0].contains("ERROR: Your run produced no reply text"),
        "first request must NOT contain the nudge"
    );
    assert!(
        requests[1].contains("ERROR: Your run produced no reply text"),
        "second request must carry the DM_EMPTY_REPLY_RETRY nudge message; got: {}",
        requests[1]
    );
}

/// B3 (#987 / #1154): an agent that keeps calling tools without ever
/// producing a final reply must be terminated by the iteration cap instead
/// of looping forever. The loop makes exactly `max_iterations` LLM calls and
/// then returns an `Err` whose message identifies the iteration limit. On a
/// DM run that `Err` is what `finish_run` wraps into `FailedWithToolCalls`,
/// which the gateway's `handle_dm_run_failure` converts into an `Errored`
/// conversation end so the peer is notified rather than stranded.
#[tokio::test]
async fn agent_loop_iteration_cap_terminates_with_error() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    // A streaming SSE chunk that always requests one `echo` tool call and
    // never produces final text — so the loop can never terminate on its
    // own and must hit the iteration cap.
    let tool_call_body = concat!(
        "data: {\"id\":\"t\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
        "\"tool_calls\":[{\"index\":0,\"id\":\"call_loop\",\"type\":\"function\",",
        "\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"message\\\":\\\"hi\\\"}\"}}]},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    // Serve the tool-call response on every accepted connection until the
    // test drops the listener. Count how many LLM calls the loop actually
    // made so we can assert it stopped at the cap.
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            call_count_writer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                tool_call_body.len(),
                tool_call_body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    const MAX_ITERS: u32 = 3;
    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        max_iterations: MAX_ITERS,
        // Disable the duration cap so this test isolates the iteration cap.
        max_run_duration_secs: 0,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    // The loop must terminate with an error (NOT loop forever, NOT Cancelled
    // — a non-Cancelled Err is what the gateway routes through
    // `handle_dm_run_failure` to notify the DM peer). `AgentLoopOutput` is
    // not `Debug`, so match rather than `expect_err`.
    let err = match result {
        Ok(_) => panic!("iteration cap must terminate the loop with an error"),
        Err(e) => e,
    };
    assert!(
        !matches!(err, alms_core::AlmsError::Cancelled),
        "the cap trip must be a failure, not a cancellation; got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("iteration") && msg.contains(&MAX_ITERS.to_string()),
        "error must identify the iteration cap; got: {msg}"
    );

    // Exactly `max_iterations` LLM calls were made — the cap fires before the
    // (would-be) 4th call.
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::SeqCst),
        MAX_ITERS as usize,
        "the loop must make exactly max_iterations LLM calls before tripping the cap"
    );

    // The sanitiser surfaces a clear, distinct label for session history /
    // the peer notification (not the generic "Runtime error").
    assert_eq!(
        alms_core::sanitize_error_for_session(&alms_core::AlmsError::Runtime(msg)),
        "Agent stopped after reaching its iteration limit"
    );
}

/// #1162 / #1163 + Codex P2: a streaming per-chunk **stall** must FALL THROUGH
/// to the buffered `complete()` fallback and can recover there. The buffered
/// path is non-streaming, so its first-byte wait (`req.send()`) is bounded by
/// the full `timeout_secs`, not the per-chunk guard — a slow-*generating* model
/// whose stream went quiet still succeeds on the re-issue. The mock stalls the
/// first (streaming) call past the 1s per-chunk window, then returns a complete
/// non-streaming JSON on the second (buffered) call; the run must succeed with
/// that content, having made exactly TWO upstream calls.
#[tokio::test]
async fn agent_loop_stalled_stream_falls_back_to_buffered_and_recovers() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    // A valid SSE prefix with no event terminator: the parser stays in "need
    // more data", and because the server then holds the connection open the
    // per-chunk idle guard fires (-> "stream stalled", not "operation timed out").
    let partial = "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\
                   \"choices\":[{\"delta\":{\"content\":\"hel";
    // The buffered (non-streaming) retry's full response.
    let buffered_body = r#"{"id":"cmpl-1","object":"chat.completion","created":1,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"recovered via buffered"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    let call_count = std::sync::Arc::new(AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            // `fetch_add` returns the prior value: 0 = streaming, 1 = buffered.
            let n = call_count_writer.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Streaming attempt: headers + partial, then stall past the 1s
                // per-chunk window so the idle guard faults it ("stream stalled").
                let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                               Content-Length: 8192\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(headers.as_bytes()).await;
                let _ = sock.write_all(partial.as_bytes()).await;
                let _ = sock.flush().await;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                drop(sock);
            } else {
                // Buffered `complete()`: one full, valid non-streaming JSON.
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    buffered_body.len(),
                    buffered_body
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        // Per-chunk window (1s) below the total deadline (30s): the streaming
        // idle guard faults the stall; the buffered retry completes well within.
        timeout_secs: 30,
        stream_chunk_timeout_secs: 1,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        max_iterations: 1000,
        max_run_duration_secs: 0,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    // The stall must NOT short-circuit: the buffered retry recovers the run.
    let output = match result {
        Ok(o) => o,
        Err(e) => panic!("a recoverable stall must succeed via the buffered fallback, got: {e}"),
    };
    assert!(
        output.response.contains("recovered via buffered"),
        "run must return the buffered retry's content; got: {:?}",
        output.response
    );
    // Defining assertion: TWO upstream calls — the stalled stream, then the
    // buffered retry that succeeded (proving the recovery the narrowing exists
    // for; pre-narrowing the stall short-circuited at one call).
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "a stall must fall through to the buffered retry (stream + buffered = 2)"
    );
}

/// #1162 / #1163: a streaming **total** timeout (`operation timed out` — the
/// whole call blew `timeout_secs`) IS genuinely futile to re-issue, so it must
/// short-circuit the buffered fallback — exactly ONE upstream call, with the
/// timeout diagnostic surfaced. (Preserves the original minimax-fix coverage,
/// now scoped to the total-timeout class per Codex P2.) The mock holds the
/// stream open while the per-chunk window stays generous, so the reqwest total
/// `.timeout()` is what faults the read.
#[tokio::test]
async fn agent_loop_total_timeout_stream_skips_futile_buffered_fallback() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let partial = "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\
                   \"choices\":[{\"delta\":{\"content\":\"hel";

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    let call_count = std::sync::Arc::new(AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            call_count_writer.fetch_add(1, Ordering::SeqCst);
            // Headers + partial, then hold open past the total deadline.
            let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                           Content-Length: 8192\r\nConnection: close\r\n\r\n";
            let _ = sock.write_all(headers.as_bytes()).await;
            let _ = sock.write_all(partial.as_bytes()).await;
            let _ = sock.flush().await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            drop(sock);
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        // Total deadline (2s) BELOW the per-chunk window (30s): the reqwest
        // total `.timeout()` faults the read first -> "operation timed out".
        timeout_secs: 2,
        stream_chunk_timeout_secs: 30,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        max_iterations: 1000,
        max_run_duration_secs: 0,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let start = std::time::Instant::now();
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;
    let elapsed = start.elapsed();

    let err = match result {
        Ok(_) => panic!("a total-timeout stream must terminate the run with an error"),
        Err(e) => e,
    };
    assert!(
        !matches!(err, alms_core::AlmsError::Cancelled),
        "a total timeout is a failure, not a cancellation; got {err:?}"
    );
    // The surfaced error is the timeout diagnostic, carrying the model.
    let msg = err.to_string();
    assert!(
        msg.contains("operation timed out"),
        "expected the total-timeout diagnostic to surface; got: {msg}"
    );
    assert!(
        msg.contains("model=test-model"),
        "the surfaced error must carry the model for attribution; got: {msg}"
    );
    // Defining assertion: exactly ONE upstream call — the buffered fallback is
    // futile for a total timeout and must be skipped.
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "a total-timeout streaming failure must NOT trigger the buffered fallback"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "total-timeout run should fail fast (one call); took {elapsed:?}"
    );
}

/// B3 (#987 / #1154): the wall-clock duration cap also terminates a wedged
/// loop, independently of the iteration cap. A mock that sleeps ~1.2s per
/// turn against a 1-second run-duration budget trips the between-iteration
/// elapsed check on the second iteration; the loop returns an error
/// identifying the time limit.
#[tokio::test]
async fn agent_loop_duration_cap_terminates_with_error() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    // Always requests one `echo` tool call, but sleeps ~1.2s before
    // responding so a 1-second duration budget is exceeded.
    let tool_call_body = concat!(
        "data: {\"id\":\"t\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
        "\"tool_calls\":[{\"index\":0,\"id\":\"call_loop\",\"type\":\"function\",",
        "\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"message\\\":\\\"hi\\\"}\"}}]},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                tool_call_body.len(),
                tool_call_body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        // Iteration cap high so it cannot be the cause; the 1-second duration
        // budget is what trips (after the first ~1.2s tool round-trip).
        max_iterations: 1000,
        max_run_duration_secs: 1,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    let err = match result {
        Ok(_) => panic!("duration cap must terminate the loop with an error"),
        Err(e) => e,
    };
    assert!(!matches!(err, alms_core::AlmsError::Cancelled));
    let msg = err.to_string();
    assert!(
        msg.contains("duration"),
        "error must identify the duration cap; got: {msg}"
    );
    assert_eq!(
        alms_core::sanitize_error_for_session(&alms_core::AlmsError::Runtime(msg)),
        "Agent stopped after reaching its time limit"
    );
}

/// B3 (#987 / #1160): `0` is the documented escape hatch — `max_iterations =
/// 0` and `max_run_duration_secs = 0` mean "no cap", NOT "trip immediately".
/// Both guards in the loop are `value > 0 && …`, so a `0` short-circuits the
/// check; this test locks that semantics in so a future refactor that drops
/// the `> 0` gate (turning `0` into an instant trip on iteration 1) is caught.
///
/// The mock serves three `echo` tool-call turns and then a final text reply,
/// so the loop makes four LLM calls. If either cap treated `0` as a trip, the
/// loop would error on the first iteration instead of completing; asserting it
/// runs to the final text proves `0` disabled both caps.
#[tokio::test]
async fn agent_loop_zero_caps_disable_both_limits() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    const REPLY_TEXT: &str = "Done after three tool turns.";

    // A streaming SSE chunk that requests one `echo` tool call (no final
    // text), used for the first three turns.
    let tool_call_body = concat!(
        "data: {\"id\":\"t\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
        "\"tool_calls\":[{\"index\":0,\"id\":\"call_loop\",\"type\":\"function\",",
        "\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"message\\\":\\\"hi\\\"}\"}}]},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    // Final turn: real text reply, no tool call — the loop terminates here.
    let final_body = format!(
        concat!(
            "data: {{\"id\":\"tf\",\"object\":\"chat.completion.chunk\",\"created\":4,",
            "\"model\":\"test-model\",\"choices\":[{{\"index\":0,",
            "\"delta\":{{\"role\":\"assistant\",\"content\":\"{}\"}},",
            "\"finish_reason\":\"stop\"}}]}}\n\n",
            "data: [DONE]\n\n"
        ),
        REPLY_TEXT
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        let bodies = [
            tool_call_body.clone(),
            tool_call_body.clone(),
            tool_call_body,
            final_body,
        ];
        for body in bodies {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            call_count_writer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        // Both caps disabled — must NOT trip on iteration 1.
        max_iterations: 0,
        max_run_duration_secs: 0,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    // With both caps at 0 the loop ran all four turns to the final text
    // instead of erroring on iteration 1 — `0` is "no cap", not "trip now".
    let output = result.expect("zero caps must disable both limits; the loop must complete");
    assert_eq!(output.response, REPLY_TEXT);
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::SeqCst),
        4,
        "the loop must make all four LLM calls — three tool turns plus the final text"
    );
}

/// Test-only tool that sleeps a fixed duration and emits no progress signal
/// while it runs. Used by the #1150 phase-aware inactivity-timer integration
/// tests to drive the `ExecutingTools` (P3) phase either past the
/// `tool_phase_ceiling_secs` backstop (a stall trip) or comfortably under it
/// (a productive run that survives). It reports `is_auto_approved() == true`
/// so it runs under the default Guarded posture with no approval round-trip —
/// exactly like the builtin `echo` the sibling B3 cap tests drive.
#[derive(Debug)]
struct SleepTool {
    ms: u64,
}

#[async_trait::async_trait]
impl alms_sandbox::Tool for SleepTool {
    fn name(&self) -> &str {
        "sleep_tool"
    }
    fn description(&self) -> &str {
        "sleeps for a fixed duration (test only)"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn execute(
        &self,
        _params: serde_json::Value,
    ) -> alms_sandbox::error::SandboxResult<serde_json::Value> {
        tokio::time::sleep(std::time::Duration::from_millis(self.ms)).await;
        Ok(serde_json::json!({"ok": true}))
    }
    fn is_auto_approved(&self) -> bool {
        true
    }
}

/// A streaming SSE body that requests exactly one `sleep_tool` call and no
/// final text, so the loop keeps iterating (the model never "finishes") and
/// the phase-aware timer is what must stop it.
const SLEEP_TOOL_SSE_BODY: &str = concat!(
    "data: {\"id\":\"t\",\"object\":\"chat.completion.chunk\",\"created\":1,",
    "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
    "\"tool_calls\":[{\"index\":0,\"id\":\"call_sleep\",\"type\":\"function\",",
    "\"function\":{\"name\":\"sleep_tool\",\"arguments\":\"{}\"}}]},",
    "\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n"
);

/// #1150 (P3 ceiling): the phase-aware inactivity timer terminates a run whose
/// tool batch overruns the `tool_phase_ceiling_secs` backstop. A `sleep_tool`
/// that runs ~1.3s — producing no token / reasoning / tool-start signal while
/// it sleeps — against a 1-second ceiling leaves the run idle past the budget,
/// so the next top-of-loop checkpoint, now in the `ExecutingTools` phase, trips
/// with the distinct "stalled" error. The iteration / wall-clock / P1 caps are
/// all disabled so this isolates P3. (No watchdog: the slow tool finishes
/// first; the run is terminated at the following checkpoint, not mid-tool.)
#[tokio::test]
async fn agent_loop_inactivity_tool_ceiling_terminates_with_error() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    // Serve the sleep-tool response on every accepted connection. Count the
    // calls so we can assert the ceiling tripped right after the first batch.
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            call_count_writer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                SLEEP_TOOL_SSE_BODY.len(),
                SLEEP_TOOL_SSE_BODY
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        // Isolate the P3 ceiling: iteration / wall-clock / P1 all disabled.
        max_iterations: 1000,
        max_run_duration_secs: 0,
        between_iterations_secs: 0,
        tool_phase_ceiling_secs: 1,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();
    // The slow tool sleeps past the 1-second ceiling.
    runtime
        .tools
        .register(std::sync::Arc::new(SleepTool { ms: 1300 }));

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    let err = match result {
        Ok(_) => panic!("the tool-phase ceiling must terminate the loop with an error"),
        Err(e) => e,
    };
    assert!(
        !matches!(err, alms_core::AlmsError::Cancelled),
        "the ceiling trip must be a failure, not a cancellation; got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("stalled") && msg.contains("executing tools"),
        "error must identify the stalled tool phase; got: {msg}"
    );

    // The ceiling tripped at the FIRST checkpoint after the slow batch, so the
    // loop made exactly one LLM call — it never reached a second turn.
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the loop must trip on the ceiling after exactly one tool round-trip"
    );

    // The sanitiser surfaces the distinct stalled label (not the iteration or
    // time-limit labels, and not the generic "Runtime error").
    assert_eq!(
        alms_core::sanitize_error_for_session(&alms_core::AlmsError::Runtime(msg)),
        "Agent stopped after stalling (no activity)"
    );
}

/// #1150 (productive run survives): the inactivity timer must NOT clip a run
/// that keeps making progress, even when the run's total wall-clock far
/// exceeds the (small) inactivity budgets. A `sleep_tool` batch of ~400ms per
/// turn stays comfortably under the 1-second P3 ceiling, and every turn
/// produces activity (the tool-call response + the tool start), so no
/// checkpoint ever trips the stall guard. Across four turns the run spans
/// ~1.6s — well past the 1-second budgets — yet it terminates on the
/// *iteration* cap, never on a stall. This is the regression guard for the
/// whole point of #1150: replacing the flat wall-clock cap with a
/// progress-aware one so long-but-productive runs are not killed.
#[tokio::test]
async fn agent_loop_inactivity_productive_run_survives() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            call_count_writer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                SLEEP_TOOL_SSE_BODY.len(),
                SLEEP_TOOL_SSE_BODY
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    const MAX_ITERS: u32 = 4;
    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        // Small inactivity budgets (1s each) and the wall-clock backstop
        // disabled, so ONLY the iteration cap can stop a productive run. If the
        // inactivity timer were progress-blind it would trip long before the
        // 4th turn; that it does not is the property under test.
        max_iterations: MAX_ITERS,
        max_run_duration_secs: 0,
        between_iterations_secs: 1,
        tool_phase_ceiling_secs: 1,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();
    // ~400ms per batch: under the 1-second ceiling, but four of them span
    // ~1.6s — comfortably past the 1-second budgets.
    runtime
        .tools
        .register(std::sync::Arc::new(SleepTool { ms: 400 }));

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    let err = match result {
        Ok(_) => panic!("the loop must terminate on the iteration cap, not run forever"),
        Err(e) => e,
    };
    let msg = err.to_string();
    // It survived every inactivity checkpoint and stopped on the iteration cap.
    assert!(
        msg.contains("iteration") && msg.contains(&MAX_ITERS.to_string()),
        "a productive run must terminate on the iteration cap, not a stall; got: {msg}"
    );
    assert!(
        !msg.contains("stalled"),
        "the inactivity timer must NOT clip a productive run; got: {msg}"
    );
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::SeqCst),
        MAX_ITERS as usize,
        "all four productive turns must run before the iteration cap trips"
    );
}
