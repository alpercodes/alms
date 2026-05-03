/// Check whether a tool result value indicates success.
///
/// Most tools return plain JSON with no `exit_code` field, which is
/// treated as success.  `shell_exec` is the exception: it embeds an
/// `exit_code` integer in the result JSON.  A non-zero exit code is
/// treated as a failure even though the tool call itself succeeded
/// (the error is semantic, not structural).
///
/// This helper centralises the check so `agent_loop` and
/// `execute_tool_call` do not need to know about `shell_exec`'s
/// return shape directly (addresses #368 feedback point 6).
pub(crate) fn tool_result_ok(value: &serde_json::Value) -> bool {
    value
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .is_none_or(|code| code == 0)
}
