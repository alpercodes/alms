# Codex Project Review

Date: 2026-03-09
Repo: `C:\dev\alms`

## Full Review Report

### Findings

1. `Critical` `POST /runs` is not actually executing against the session the caller selected. `create_run()` resolves the requested session in `crates/alms-gateway/src/runs.rs`, but `execute_run()` passes `session_id.to_string()` as the runtime context, and `AgentRuntime::run()` turns that into a fresh `(agent_id, context_id)` key via `get_or_create()`. The effect is that HTTP and scheduled runs write history into a hidden session keyed by the UUID, so the requested session's history and summaries drift or stay empty.

2. `High` The default tool surface is effectively unsandboxed. The only filesystem gate is `check_no_traversal()` in `crates/alms-sandbox/src/builtin.rs`, which explicitly allows absolute paths; `http_get` has no host allowlist; `shell_exec` accepts arbitrary `argv` and `cwd`; and every runtime starts with all builtins enabled. That is much closer to raw local access than to the scoped and capability-based model the project claims.

3. `High` Agent identity is ephemeral per process start. `Gateway::new()` generates a fresh `AgentId` on every boot in `crates/alms-gateway/src/gateway.rs`, `/settings` publishes that ID to clients in `crates/alms-gateway/src/settings.rs`, and Telegram history is keyed off that runtime in `crates/alms-gateway/src/gateway.rs`. A restart therefore fragments sessions and workspaces and can re-trigger bootstrap instead of continuing the same agent.

4. `High` Telegram startup is broken when a bot token is configured. `post()` already unwraps the outer Telegram envelope in `crates/alms-channel/src/telegram/mod.rs`, but `set_webhook()` and `delete_webhook()` try to deserialize a second `TelegramResponse<bool>`, and `start()` always calls one of those methods. In practice the Telegram channel should fail before polling or webhook setup completes.

5. `Medium` The scheduler's core heap logic is incorrect. `run_loop()` picks the "next" job via `BinaryHeap::iter().find(...)`, but heap iteration is unordered, and `process_due_jobs()` still fires due jobs without filtering cancelled IDs. Under mixed loads, earlier jobs can be delayed and cancelled jobs can still run once another wake-up happens.

6. `Medium` SSE delivery is lossy and only supports one live subscriber. `stream_run_events()` replays the log before registering the live sender, so events emitted in that gap are missed, and `RunManager` stores a single sender per run, so a second subscriber replaces the first.

7. `Medium` Session persistence drifts after restart. `append_message()` persists the message row but only updates `last_activity` in memory, and `archive_idle()` changes `status` only in memory. SQLite therefore reloads stale activity, ordering, and archive state even when message history itself was saved.

### Take

The crate split is good and the code is readable, but the implementation is still breaking its own core promises around session identity, stable agent identity, and bounded tool access. I would not call the current project secure or persistent yet; I would fix findings 1 through 3 first, then add regression coverage around run and session history, Telegram startup, scheduler ordering and cancellation, and SSE subscribe and reconnect.

### Verification

`cargo test --workspace` passed on the current worktree.

`cargo clippy --workspace --all-targets -- -D warnings` currently stops on unused imports in:

- `crates/alms-session/src/sqlite.rs`
- `crates/alms-gateway/tests/sse_golden_tests.rs`

## Assistant Responses In This Session

Below is the response log I sent during this session, reconstructed into one place so it is readable from the repo.

1. Reviewing the project as a code review pass. I'll map the repo first, identify the main runtime paths and test surface, then read the highest-risk areas before writing findings.

2. The initial shell calls failed before returning output, likely from the shell startup path rather than the repo itself. I'm retrying with a minimal non-login shell and then I'll inspect the app structure from there.

3. PowerShell itself appears to be failing to initialize in this session. I'm switching to `cmd`-invoked commands through the same tool so I can still inspect the repository.

4. The shell runner is still failing before command execution. I'm retrying one read-only command outside the sandbox to determine whether this is a sandbox startup issue or a broader session problem.

5. The workspace is a Rust monorepo with gateway, runtime, sandbox, session, coordinator, channel, CLI, and core crates. There are active local edits in several runtime, gateway, and sandbox files, so I'm treating the review as against the current worktree, not just `HEAD`.

6. I've got the top-level design and crate layout. Next I'm sizing the source surface and reading the central runtime, gateway, sandbox, and session modules first, because that's where correctness and security risk concentrate.

7. The codebase is implementation-heavy in a few files rather than spread uniformly. I'm into the runtime and storage internals now, then I'll validate the gateway and API layer against those behaviors.

8. I've reached the sandbox boundary code. Now I'm reading the actual `shell_exec` and filesystem implementations with line numbers, because that's where the project's safety claims either hold or fall apart.

9. The static review has already surfaced a few likely correctness and security problems. I'm running the test suite now to see which of those are already covered and whether there are additional runtime failures the code paths don't reveal on inspection.

10. The test suite passes, but that only tells us the current tests aren't exercising the failure modes I'm seeing. I'm doing one more mechanical pass with `clippy` to catch unused and error-prone constructs before I write the review.

11. One structural issue is now clear: the gateway's run path is not actually executing against the session the API caller selected. I'm pulling exact line references for the runtime and session boundary before I finalize the review.

12. I found a scheduler correctness bug that the current tests don't catch. I'm pulling the exact lines now, then I should have enough to write the review cleanly.

13. Final response sent in chat:

    Findings

    1. `Critical` `POST /runs` is not actually executing against the session the caller selected. `create_run()` resolves the requested session in [runs.rs](/C:/dev/alms/crates/alms-gateway/src/runs.rs#L59), but `execute_run()` passes `session_id.to_string()` as the runtime context in [runs.rs](/C:/dev/alms/crates/alms-gateway/src/runs.rs#L247), and `AgentRuntime::run()` turns that into a fresh `(agent_id, context_id)` key via `get_or_create()` in [agent.rs](/C:/dev/alms/crates/alms-runtime/src/agent.rs#L144). The effect is that HTTP and scheduled runs write history into a hidden session keyed by the UUID, so the requested session's history/summaries drift or stay empty.

    2. `High` The default tool surface is effectively unsandboxed. The only filesystem gate is `check_no_traversal()` in [builtin.rs](/C:/dev/alms/crates/alms-sandbox/src/builtin.rs#L7), which explicitly allows absolute paths; `http_get` has no host allowlist in [builtin.rs](/C:/dev/alms/crates/alms-sandbox/src/builtin.rs#L320); `shell_exec` accepts arbitrary `argv` and `cwd` in [builtin.rs](/C:/dev/alms/crates/alms-sandbox/src/builtin.rs#L443); and every runtime starts with all builtins enabled in [agent.rs](/C:/dev/alms/crates/alms-runtime/src/agent.rs#L80). That is much closer to raw local access than to the scoped/capability model the project claims.

    3. `High` Agent identity is ephemeral per process start. `Gateway::new()` generates a fresh `AgentId` on every boot in [gateway.rs](/C:/dev/alms/crates/alms-gateway/src/gateway.rs#L115), `/settings` publishes that ID to clients in [settings.rs](/C:/dev/alms/crates/alms-gateway/src/settings.rs#L33), and Telegram history is keyed off that runtime in [gateway.rs](/C:/dev/alms/crates/alms-gateway/src/gateway.rs#L233). A restart therefore fragments sessions/workspaces and can re-trigger bootstrap instead of continuing the same agent.

    4. `High` Telegram startup is broken when a bot token is configured. `post()` already unwraps the outer Telegram envelope in [mod.rs](/C:/dev/alms/crates/alms-channel/src/telegram/mod.rs#L70), but `set_webhook()` and `delete_webhook()` try to deserialize a second `TelegramResponse<bool>` in [mod.rs](/C:/dev/alms/crates/alms-channel/src/telegram/mod.rs#L134), and `start()` always calls one of those methods in [mod.rs](/C:/dev/alms/crates/alms-channel/src/telegram/mod.rs#L305). In practice the Telegram channel should fail before polling/webhook setup completes.

    5. `Medium` The scheduler's core heap logic is incorrect. `run_loop()` picks the "next" job via `BinaryHeap::iter().find(...)` in [scheduler.rs](/C:/dev/alms/crates/alms-runtime/src/scheduler.rs#L160), but heap iteration is unordered, and `process_due_jobs()` still fires due jobs without filtering cancelled IDs in [scheduler.rs](/C:/dev/alms/crates/alms-runtime/src/scheduler.rs#L194). Under mixed loads, earlier jobs can be delayed and cancelled jobs can still run once another wake-up happens.

    6. `Medium` SSE delivery is lossy and only supports one live subscriber. `stream_run_events()` replays the log before registering the live sender in [runs.rs](/C:/dev/alms/crates/alms-gateway/src/runs.rs#L492), so events emitted in that gap are missed, and `RunManager` stores a single sender per run in [server.rs](/C:/dev/alms/crates/alms-gateway/src/server.rs#L38), so a second subscriber replaces the first.

    7. `Medium` Session persistence drifts after restart. `append_message()` persists the message row but only updates `last_activity` in memory in [lib.rs](/C:/dev/alms/crates/alms-session/src/lib.rs#L129), and `archive_idle()` changes `status` only in memory in [lib.rs](/C:/dev/alms/crates/alms-session/src/lib.rs#L210). SQLite therefore reloads stale activity/order/archive state even when message history itself was saved.

    Take

    The crate split is good and the code is readable, but the implementation is still breaking its own core promises around session identity, stable agent identity, and bounded tool access. I would not call the current project "secure" or "persistent" yet; I'd fix findings 1-3 first, then add regression coverage around run/session history, Telegram startup, scheduler ordering/cancellation, and SSE subscribe/reconnect.

    Verification

    `cargo test --workspace` passed on the current worktree. `cargo clippy --workspace --all-targets -- -D warnings` currently stops on unused imports in [sqlite.rs](/C:/dev/alms/crates/alms-session/src/sqlite.rs#L563) and [sse_golden_tests.rs](/C:/dev/alms/crates/alms-gateway/tests/sse_golden_tests.rs#L7).
