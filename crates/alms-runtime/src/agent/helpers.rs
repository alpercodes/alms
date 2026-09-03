// SPDX-License-Identifier: Apache-2.0

/// Check whether a tool result value indicates success.
///
/// The runtime's notion of "did this tool call succeed?" feeds three
/// downstream consumers, all of which are operator-visible:
///
/// 1. The `tool_end` SSE event's `ok` field — the web UI maps `ok=false`
///    to the red row styling (`tc-fail`) in `tool-row.js`.
/// 2. The persisted `metadata.ok` flag on the tool-result message — the
///    UI's history rehydration path (`history.js`) uses it to colour
///    pre-existing rows on page reload.
/// 3. The agent's own `tool_result` reading on the next turn — `ok=false`
///    gives the model a structured cue that the tool call did not succeed.
///
/// Most tools return plain JSON with no special status fields, which is
/// treated as success. The recognised failure shapes are:
///
/// - `value.exit_code` is a non-zero integer (`shell` / `shell_exec`).
/// - `value.error` is a string with non-empty content. This is the
///   "in-band error" convention used by `send_message` (agent-not-found,
///   depth exceeded, self-message), `read_session` / `read_subagent_session`
///   / `read_messages` (not-found, access denied), and the background
///   `shell` task `status: "failed"` payload. Only non-empty string `error`
///   values are recognised — a structured payload like
///   `{"error": {"code": 404, "message": "..."}}` falls through to success.
///   No first-party tool emits that shape today; if a future tool needs to
///   return structured errors, extend the discriminator here.
/// - `value.status` is a number ≥ 400 (`http_get`) — HTTP 4xx/5xx
///   responses come back as `Ok({"status": 503, "body": ..., ...})`
///   because the *transport* succeeded; only the response status
///   classifies it as a failure for the operator's at-a-glance signal.
/// - `value.status` is the string `"failed"` or `"error"` — used by the
///   background-task report shape that omits `exit_code` when the task
///   failed to spawn or was cancelled (`{status: "failed", error, ...}`).
///
/// Passthrough-tool caveat: `echo` (and any future tool that returns
/// caller-shaped JSON verbatim) can be coerced into tripping every
/// discriminator above by calling it with a `message` whose value
/// contains a matching top-level field — e.g.
/// `echo({"message": {"error": "x"}})` unwraps to `{"error": "x"}` and
/// flips `ok=false`; the same vector applies to `{"status": 404}`
/// (integer-status branch) and `{"status": "failed"}` (string-status
/// branch). A grep of `alms-tools/` and `alms-sandbox/builtin/` confirms
/// that `http_get` is the only tool that legitimately emits a top-level
/// integer `status`, and the in-band `error` / string-`status` shapes
/// are all owned by first-party tools, so the discriminator is sound
/// for every production caller. Echo is a debugging tool with no
/// production caller, so the coercion is accepted; a future first-class
/// passthrough tool would need either a per-tool exemption or a fixed
/// result envelope so the discriminator can't alias caller payload into
/// a failure signal.
///
/// This helper centralises the check so `agent_loop` and
/// `execute_tool_call` do not need to know about per-tool return shapes
/// directly (addresses #368 feedback point 6; expanded for #1048 to
/// recognise the in-band error / HTTP-status / background-failure
/// conventions).
pub(crate) fn tool_result_ok(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        // Non-object results (plain strings from `echo`, JSON null,
        // arrays, numbers) carry no failure signal and are treated as
        // success. The runtime's `Err(_)` branch in `execute_tool_call`
        // is the only path that flags a tool-level failure for these
        // shapes.
        return true;
    };

    // Shell `exit_code` discriminator. A non-i64 exit_code (e.g. string,
    // float) is ignored — `as_i64()` returns None and the call falls
    // through to the other checks below rather than being silently
    // treated as success.
    if let Some(code) = obj.get("exit_code").and_then(|v| v.as_i64())
        && code != 0
    {
        return false;
    }

    // User-denial discriminator (#1109): the approval-gate deny branch
    // returns `{"user_denied": true, ...}` — deliberately distinct from
    // the `error` shape so the agent can tell user policy from runtime
    // failure, but still a failed call for `ok`-flag purposes. Only an
    // explicit boolean `true` matches.
    if obj.get("user_denied").and_then(|v| v.as_bool()) == Some(true) {
        return false;
    }

    // In-band `error` string. The convention used by `send_message`,
    // `read_session`, `read_subagent_session`, `read_messages`, and the
    // background-shell `status: "failed"` payload (see callers in
    // `crates/alms-tools/src/*.rs` and `crates/alms-sandbox/src/shell/mod.rs`).
    // Empty strings are deliberately ignored so a tool that returns
    // `{"error": ""}` (no actual error) doesn't get mis-flagged.
    if obj
        .get("error")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        return false;
    }

    // HTTP-status discriminator for `http_get`. The tool wraps every
    // response — including 4xx / 5xx — in `Ok({"status": N, ...})`
    // because the underlying transport succeeded; only the response
    // status tells the operator whether the *job* succeeded.
    //
    // `http_get` only emits integer statuses in the 100–599 range, so a
    // bare ≥ 400 threshold is enough; the floor avoids spuriously
    // failing tools that happen to carry an unrelated `status` integer
    // field (none today, but future first-party tools might).
    if let Some(status) = obj.get("status").and_then(|v| v.as_i64())
        && status >= 400
    {
        return false;
    }

    // Background-shell string-status discriminator. The background-task
    // failure payload is `{status: "failed", error: "...", task_id: ...}`
    // — but the `error`-string check above already catches that case.
    // This branch covers the defensive corner where a future caller
    // emits `status: "failed"` without an accompanying `error` field.
    if let Some(s) = obj.get("status").and_then(|v| v.as_str())
        && (s.eq_ignore_ascii_case("failed") || s.eq_ignore_ascii_case("error"))
    {
        return false;
    }

    true
}
