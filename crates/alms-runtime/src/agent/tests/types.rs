// SPDX-License-Identifier: Apache-2.0

//! `Posture` parsing, `is_user_facing_context`, and the `tool_result_ok` in-band error conventions.

use crate::agent::*;

#[test]
fn test_posture_from_str() {
    assert_eq!("guarded".parse::<Posture>().unwrap(), Posture::Guarded);
    assert_eq!(
        "full_control".parse::<Posture>().unwrap(),
        Posture::FullControl
    );
    assert_eq!(
        "autonomous".parse::<Posture>().unwrap(),
        Posture::Autonomous
    );
    assert!("unknown".parse::<Posture>().is_err());
    assert!("".parse::<Posture>().is_err());
}

#[test]
fn test_posture_display() {
    assert_eq!(Posture::Guarded.to_string(), "guarded");
    assert_eq!(Posture::FullControl.to_string(), "full_control");
    assert_eq!(Posture::Autonomous.to_string(), "autonomous");
}

#[test]
fn test_posture_roundtrip() {
    for posture in [Posture::Guarded, Posture::FullControl, Posture::Autonomous] {
        let s = posture.to_string();
        let parsed: Posture = s.parse().unwrap();
        assert_eq!(parsed, posture);
    }
}

#[test]
fn test_is_user_facing_context() {
    // User-facing: web UI, Telegram
    assert!(AgentRuntime::is_user_facing_context("web-chat-123"));
    assert!(AgentRuntime::is_user_facing_context("telegram_agent_456"));

    // Non-user-facing: DM, subagent, job, notification
    assert!(!AgentRuntime::is_user_facing_context("dm:alice:bob"));
    assert!(!AgentRuntime::is_user_facing_context("subagent_task123"));
    assert!(!AgentRuntime::is_user_facing_context(
        "subagent_task123_reviewer"
    ));
    assert!(!AgentRuntime::is_user_facing_context("job_abc"));
    assert!(!AgentRuntime::is_user_facing_context("notifications:alice"));
    assert!(!AgentRuntime::is_user_facing_context(
        "notifications:my-agent"
    ));

    // Edge cases: empty string and unknown prefix default to user-facing
    assert!(AgentRuntime::is_user_facing_context(""));
    assert!(AgentRuntime::is_user_facing_context("unknown_prefix"));

    // Near-miss prefixes must NOT match (prefix must be exact)
    assert!(AgentRuntime::is_user_facing_context("dmx:something"));
    assert!(AgentRuntime::is_user_facing_context("subagentx_something"));
    assert!(AgentRuntime::is_user_facing_context("jobs_something"));
    assert!(AgentRuntime::is_user_facing_context(
        "notification_something"
    ));
}

// ── tool_result_ok tests ─────────────────────────────────────────────────

#[test]
fn test_tool_result_ok_no_exit_code() {
    // Most tools return plain JSON without exit_code — treated as success.
    let val = serde_json::json!({"output": "hello"});
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_exit_code_zero() {
    let val = serde_json::json!({"exit_code": 0, "stdout": "ok"});
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_exit_code_nonzero() {
    let val = serde_json::json!({"exit_code": 1, "stderr": "fail"});
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_exit_code_negative() {
    // Killed by signal (e.g. -9) should also be failure.
    let val = serde_json::json!({"exit_code": -9});
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_exit_code_non_integer() {
    // exit_code that is not an integer (e.g. string) is treated as success
    // because `as_i64()` returns None for non-integer JSON values.
    let val = serde_json::json!({"exit_code": "not_a_number"});
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_string_value() {
    // Plain string result (e.g. echo tool) is always success.
    let val = serde_json::json!("just a string");
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_null_value() {
    let val = serde_json::Value::Null;
    assert!(helpers::tool_result_ok(&val));
}

// #1048 — additional in-band error conventions the runtime now treats
// as failure so the operator's at-a-glance row colour matches the
// tool's semantic outcome.

#[test]
fn test_tool_result_ok_in_band_error_field() {
    // `send_message`, `read_session`, `read_subagent_session`, and
    // `read_messages` all emit `{"error": "<message>", ...}` on the
    // `Ok(...)` path for in-band failures (agent-not-found, no DM
    // session, depth exceeded, access denied, etc.). Pre-#1048 the
    // runtime classified these as success because there was no
    // `exit_code` — fix should now flag them.
    let val = serde_json::json!({"error": "Agent 'X' not found. Use list_agents to see available agents."});
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_in_band_error_empty_string_ignored() {
    // Defensive: empty-string error fields shouldn't trigger a
    // failure classification — they don't carry an actual error.
    let val = serde_json::json!({"error": "", "ok": true});
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_in_band_error_non_string_ignored() {
    // A non-string `error` field (e.g. an HTTP `error` integer code,
    // or an `error` boolean) isn't the in-band-error convention this
    // helper targets — fall through to the other checks.
    let val = serde_json::json!({"error": 0});
    assert!(helpers::tool_result_ok(&val));
    let val2 = serde_json::json!({"error": null});
    assert!(helpers::tool_result_ok(&val2));
}

#[test]
fn test_tool_result_ok_http_status_5xx() {
    // `http_get` returns `Ok({"status": 503, "body": ...})` for an
    // upstream 5xx response — the transport succeeded but the job
    // failed. The UI should render this red.
    let val = serde_json::json!({
        "status": 503,
        "content_type": "text/plain",
        "headers": {},
        "body": "Service Unavailable",
    });
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_http_status_4xx() {
    let val = serde_json::json!({
        "status": 404,
        "content_type": "text/plain",
        "headers": {},
        "body": "Not Found",
    });
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_http_status_2xx_success() {
    let val = serde_json::json!({
        "status": 200,
        "content_type": "application/json",
        "headers": {},
        "body": {"ok": true},
    });
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_http_status_3xx_success() {
    // 3xx redirects classify as success — `http_get` doesn't follow
    // them by default and a 301/302 is informational, not a failure
    // of the operator's job.
    let val = serde_json::json!({"status": 301, "body": ""});
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_background_shell_failed_status() {
    // Background `shell` tasks that fail to spawn emit
    // `{status: "failed", error: "...", task_id: ...}` (no
    // `exit_code`). The in-band `error` check catches this; the
    // status-string check is the defensive fallback.
    let val = serde_json::json!({
        "task_id": "bg_3",
        "status": "failed",
        "command": "foo",
        "error": "spawn error",
    });
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_background_shell_failed_without_error_field() {
    // Defensive fallback: a future caller that emits `status: "failed"`
    // without an `error` field should still classify as failure.
    let val = serde_json::json!({"task_id": "bg_4", "status": "failed"});
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_background_shell_submitted_status() {
    // Background submission is a success — the operator initiated the
    // task successfully even though it hasn't completed yet.
    let val = serde_json::json!({
        "task_id": "bg_1",
        "status": "submitted",
        "message": "Task started",
    });
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_background_shell_completed_with_exit_code() {
    // Background task that completed cleanly — `exit_code: 0` plus
    // `status: "completed"`. Treated as success.
    let val = serde_json::json!({
        "task_id": "bg_2",
        "status": "completed",
        "command": "echo hi",
        "exit_code": 0,
        "stdout": "hi\n",
        "stderr": "",
    });
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_background_shell_completed_with_nonzero_exit() {
    // Background task that completed with non-zero exit — the
    // `exit_code` discriminator wins over the success-flavoured
    // `status: "completed"` because the underlying command failed.
    let val = serde_json::json!({
        "task_id": "bg_2",
        "status": "completed",
        "command": "false",
        "exit_code": 1,
        "stdout": "",
        "stderr": "",
    });
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_status_string_unrecognised() {
    // String statuses other than "failed" / "error" don't flip the
    // classifier — `submitted`, `unknown`, `not_found_or_still_running`,
    // and `completed` all carry no failure signal on their own.
    for status in [
        "submitted",
        "unknown",
        "not_found_or_still_running",
        "completed",
        "in_progress",
    ] {
        let val = serde_json::json!({"status": status});
        assert!(
            helpers::tool_result_ok(&val),
            "status={status:?} should not flip the classifier"
        );
    }
}

#[test]
fn test_tool_result_ok_status_string_error_case_insensitive() {
    // Defensive: case shouldn't matter for the status-string check.
    for s in ["error", "ERROR", "Error", "failed", "FAILED", "Failed"] {
        let val = serde_json::json!({"status": s});
        assert!(
            !helpers::tool_result_ok(&val),
            "status={s:?} should classify as failure"
        );
    }
}

#[test]
fn test_tool_result_ok_user_denied() {
    // #1109: the approval-gate deny body `{"user_denied": true, ...}` is
    // a failed tool call for `ok`-flag purposes even though it carries no
    // `error` key (deliberately — the distinct key is what lets the agent
    // tell a user policy decision apart from a runtime error).
    let val = crate::agent::loop_impl::user_denied_result();
    assert!(!helpers::tool_result_ok(&val));

    // Only an explicit boolean `true` flips the classifier.
    for benign in [
        serde_json::json!({"user_denied": false}),
        serde_json::json!({"user_denied": "true"}),
        serde_json::json!({"user_denied": 1}),
    ] {
        assert!(
            helpers::tool_result_ok(&benign),
            "user_denied={benign:?} should not classify as failure"
        );
    }
}
