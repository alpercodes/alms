# ALMS Codebase Review

Date: 2026-03-13

## Scope and method

This review is based primarily on the current Rust source under `crates/`. I used the existing docs only as secondary context because several of them appear older than the implementation. I also attempted to run the workspace test suite twice, but the environment blocked Cargo from writing build artifacts with `Access is denied (os error 5)`, both under the default `target/` tree and under `data/cargo-target/`.

## Executive summary

The codebase has a solid core shape: the crate boundaries are sensible, the session queue and scheduler are thoughtfully implemented, and the runtime/gateway split is workable. The main problems are not low-level Rust quality issues; they are identity and configuration mismatches that make the newer multi-agent/workspace model inconsistent at runtime.

The most serious problems are:

1. top-level agents do not agree on where their workspace lives,
2. "default agent" changes do not appear to control the live gateway runtime,
3. parts of the unified config are loaded but never actually applied.

Those issues are structural enough that they will create confusing user-facing behavior even when the system is otherwise healthy.

## Findings

### 1. High: top-level agent workspaces are written under one identity and read under another

Evidence:

- `crates/alms-gateway/src/agents.rs:186-189` creates initial workspace files at `workspace_dir.join(&agent.name)`.
- `crates/alms-runtime/src/workspace.rs:65-69` and `crates/alms-runtime/src/workspace.rs:93-100` define the normal top-level workspace path as `{base_dir}/{agent_id}`.
- `crates/alms-gateway/src/runs.rs:255-279` uses `AgentWorkspace::new(workspace_dir, agent_id)` for bootstrap checks and runtime attachment.
- `crates/alms-gateway/src/workspace.rs:36` and `crates/alms-gateway/src/workspace.rs:95` also use `AgentWorkspace::new(workspace_dir, agent_id)` for the HTTP read/write API.

Impact:

- Creating an agent initializes files under `workspace/<agent-name>/`, but the runtime and workspace API look under `workspace/<agent-id>/`.
- A newly created agent can therefore look "uninitialized" to the runtime even though files were created successfully.
- The bootstrap prompt can be re-triggered for an agent that already has workspace files, just in the wrong directory.
- The user-facing workspace API is disconnected from the files created during agent creation.

Recommendation:

- Pick one stable top-level workspace identity and apply it everywhere.
- If the intent is human-readable paths, use the agent name consistently and store/resolve it through the registry.
- If the intent is stable UUID paths, create and edit those UUID-based directories everywhere, including agent creation.
- Add a one-time migration because existing workspaces are already at risk of being split across both layouts.

### 2. High: changing the default agent does not appear to change live execution behavior

Evidence:

- `crates/alms-gateway/src/gateway.rs:90-107` always resolves the gateway's startup `agent_id` from `./data/agent_id` (or `ALMS_AGENT_ID`), not from the agent registry's current default row.
- `crates/alms-gateway/src/agents.rs:295-310` updates the SQLite default via `set_default_agent`, but does not update any live gateway state.
- `crates/alms-gateway/src/server.rs:243-257` snapshots `default_agent_id: agent_id` once when `AppState` is built.
- `crates/alms-gateway/src/settings.rs:32` returns that cached `default_agent_id`.
- `crates/alms-gateway/src/gateway.rs:265-269` builds the channel runtime with `self.agent_id`, which is also fixed at startup.

Impact:

- This makes the "default agent" feature look mostly cosmetic for live gateway behavior.
- A `POST /agents/{id_or_name}/default` call updates the registry, but the running gateway still appears pinned to the startup sidecar/env agent ID.
- The settings endpoint can return a stale default agent ID even after the registry default changes.
- Telegram/channel-driven traffic is especially affected because it uses the gateway's fixed `self.agent_id`.

This is an inference from the current source flow rather than a directly executed runtime trace, but the code path is clear.

Recommendation:

- Make the registry's current default agent the canonical source of truth for top-level routing.
- If the sidecar file still matters for migration, use it only as a bootstrap fallback when the registry is empty.
- Stop caching `default_agent_id` as immutable process state, or refresh it when default-agent mutations happen.

### 3. Medium: the unified config is only partially wired into the actual gateway

Evidence:

- `crates/alms-core/src/config.rs:18-24` defines `AlmsConfig.session` as a first-class config section.
- `crates/alms-gateway/src/gateway.rs:67-82` ignores that section and hard-resets `session_config` to `SessionConfig::default()`.
- `crates/alms-core/src/config.rs:117-118` loads `ALMS_BIND` into `config.server.bind`.
- `crates/alms-cli/src/main.rs:29-32` hardcodes the gateway bind default to `127.0.0.1:8080`.
- `crates/alms-cli/src/main.rs:129-152` passes the CLI arg straight into `alms_gateway::serve(&bind)` instead of using the loaded config value.

Impact:

- Operators can set `session.*` in config and get no behavior change.
- `server.bind` / `ALMS_BIND` is loaded into config, but the main startup path ignores it unless the user manually passes `--bind`.
- This weakens confidence in the "single source of truth" config model because some sections are real and some are not.

Recommendation:

- Plumb `config.session` into `GatewayConfig::from_alms_config`.
- Decide whether `server.bind` belongs in config or only on the CLI, then remove the duplicate source.
- Add a small integration test that asserts an `alms.toml` value materially changes runtime behavior.

### 4. Medium: failed runs do not persist the triggering user message or any failure marker into session history

Evidence:

- `crates/alms-runtime/src/agent.rs:198-204` builds context and runs the agent loop.
- `crates/alms-runtime/src/agent.rs:206-223` appends the user and assistant messages only after `agent_loop(...)` returns `Ok(...)`.
- Any error before that point returns early from `run(...)` and skips history persistence entirely.

Impact:

- A run that fails during LLM execution, tool execution, approval handling, or context summarization leaves no conversational trace in the session history.
- The user input that caused the failure disappears from the long-term session context.
- Retrying after a failure can behave oddly because the session history does not reflect what was already attempted.

Recommendation:

- Append the user message before starting the loop.
- On failure, append a synthetic assistant/error message or a structured system/tool-failure marker.
- Keep the run record and session transcript aligned so debugging and summarization stay coherent.

### 5. Medium: the SSE event log is described as durable/persistent, but it is only in-memory process state

Evidence:

- `crates/alms-gateway/src/event_log.rs:1` describes the event log as "durable SSE event storage".
- `crates/alms-gateway/src/server.rs:44-45` describes it as a "Persistent event log for reconnect-after-restart support".
- `crates/alms-gateway/src/event_log.rs:21-23` stores events in `Arc<RwLock<Vec<LoggedEvent>>>`.
- `crates/alms-gateway/src/event_log.rs:57-64` stores per-run logs in an in-memory `HashMap<RunId, EventLog>`.

Impact:

- Replay works only within the lifetime of the current gateway process.
- Reconnect-after-restart support is not actually implemented.
- The comments overstate the reliability of the SSE replay model, which will mislead future contributors and operators.

Recommendation:

- Either persist replayable events in SQLite/file storage, or
- explicitly downgrade the comments and docs to "in-memory replay during the current process lifetime".

### 6. Medium: `GET /runs/{id}/events` can leak sender state for finished or invalid runs

Evidence:

- `crates/alms-gateway/src/runs.rs:598-605` always registers a live sender before checking anything about run state.
- `crates/alms-gateway/src/server.rs:93-99` shows senders are only explicitly removed by `remove_senders`.
- `crates/alms-gateway/src/server.rs:150-164` prunes dead senders only during future `send_event(...)` calls.
- `crates/alms-gateway/src/runs.rs:365` removes senders when a run ends, but that cleanup has already happened if a client subscribes after completion.

Impact:

- A client can subscribe to a nonexistent run ID and get an open SSE stream instead of a 404.
- Subscriptions opened after a run has already finished can leave `event_senders` entries behind, because no future event emission happens to prune them.
- Repeated invalid or post-completion subscriptions will accumulate stale sender state.

This is also an inference from the source behavior rather than a live reproduction, but the lifecycle is clear from the registration and cleanup paths.

Recommendation:

- Return 404 from `/runs/{id}/events` when the run does not exist.
- Do not register a live sender for terminal runs unless the server expects future events.
- Add disconnect-aware cleanup or periodic pruning for orphaned sender entries.

## Additional risks worth tracking

- `crates/alms-gateway/src/auth.rs:19-20` and `crates/alms-gateway/src/auth.rs:40-45` allow `?token=...` query auth for every protected route, not just SSE. That is convenient, but it also means bearer tokens can leak into URLs, logs, browser history, and referrers.
- The sidecar file `./data/agent_id` is still more authoritative than the registry default in the current gateway startup path. That is fine as a migration bridge, but dangerous if treated as a long-term coexistence model.

## Strengths

- The session queue in `crates/alms-gateway/src/session_queue.rs` is well-designed: per-key FIFO execution, idle cleanup, shutdown draining, and good tests.
- The scheduler in `crates/alms-runtime/src/scheduler.rs` is simple, testable, and uses `tokio::time` correctly for deterministic tests.
- The SQLite layer in `crates/alms-session/src/sqlite.rs` has solid coverage for session, audit, job, and agent basics, and it uses transactions in the right places (`delete_session`, `set_default_agent`).
- The runtime/gateway event separation is generally clean. Tool execution, approvals, and streaming are easier to reason about because the code keeps those concerns distinct.

## Suggested priority order

1. Fix the workspace identity split.
2. Make the registry default agent actually drive live gateway behavior.
3. Wire the config model end to end, or reduce it to only the parts that are real.
4. Make failed runs write durable session history.
5. Decide whether SSE replay is truly durable, then implement or rename it.
6. Tighten the SSE subscription lifecycle and auth query-token scope.

## Verification notes

- `cargo test` could not be completed in this environment because Cargo artifact writes failed with `Access is denied (os error 5)`.
- I therefore treated this as a source review with partial runtime inference, not as a fully executed integration review.
