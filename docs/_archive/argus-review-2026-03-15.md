# Argus Review - 2026-03-15

## Scope

Reviewed `main` at commit `4012df8` on 2026-03-15.

Validation performed:
- `cargo test`
- `cargo clippy --all-targets --all-features -- -D warnings`

Review method:
- Treated source code as primary truth.
- Used docs as secondary evidence, weighted by last-modified date and whether they matched current code.
- When docs and code conflicted, favored code unless the doc was equally recent and clearly authoritative.

## Doc freshness weighting

- `CLAUDE.md` last changed on 2026-03-15.
- `docs/api.md` last changed on 2026-03-15.
- `README.md` last changed on 2026-03-14.
- `docs/TASKS.md` last changed on 2026-03-14.
- `docs/index.md` last changed on 2026-02-14.
- `docs/testing-strategy.md` last changed on 2026-02-11.

Practical takeaway:
- `CLAUDE.md` and current Rust code are the closest thing to authoritative.
- `docs/index.md`, `docs/testing-strategy.md`, and older design docs are useful context, but not safe to treat as current behavior without checking code.

## Findings

### 1. High - `[tools].enabled` is not enforced, so `shell_exec` and filesystem tools stay exposed even when config suggests otherwise

Evidence:
- `alms.toml.example:52-58` advertises `[tools].enabled`, `timeout_secs`, and `max_output_bytes`.
- `crates/alms-core/src/config.rs:331-351` defines those fields in the unified config.
- `crates/alms-gateway/src/gateway.rs:67-83` only forwards `sandbox_root` and `shell_policy` into `AgentConfig`; it drops `enabled`, `timeout_secs`, and `max_output_bytes`.
- `crates/alms-runtime/src/agent.rs:123` always builds the runtime with `ToolRegistry::with_builtins_sandboxed(...)`.
- `crates/alms-sandbox/src/registry.rs:165-205` unconditionally registers all builtin tools, including `shell_exec`, `fs_read`, `fs_write`, and `fs_list`.
- `crates/alms-gateway/src/settings.rs:16-27` also advertises those tools to the UI regardless of `[tools].enabled`.

Impact:
- Operators can believe they have disabled dangerous tools when they have not.
- This is a config-to-security-boundary mismatch, not just a cosmetic docs issue.

Missing coverage:
- I found no test asserting that a restricted tool set actually removes tools from the runtime or from `/settings`.

### 2. High - stalled LLM streams are treated as a clean end-of-stream, so ALMS can persist truncated replies as successful output — **FIXED (PR #175)**

Evidence:
- `crates/alms-runtime/src/llm_client.rs:204-206` explicitly says stalled streams are terminated after 60 seconds.
- ~~`crates/alms-runtime/src/llm_client.rs:257-262` logs a warning and returns `None` on timeout instead of returning an error.~~
- ~~`crates/alms-runtime/src/agent.rs:675-743` accepts normal stream termination and returns whatever content/tool-call fragments were accumulated so far.~~

Impact:
- ~~A hung provider connection can produce a partial assistant reply that looks successful.~~
- ~~A partially streamed tool call can also be returned with incomplete JSON arguments.~~
- ~~Because this path does not raise an error, the session history and user-visible run result can silently diverge from what the model intended to send.~~

Fix: Stream timeout now yields `Err(AlmsError::Runtime(...))` instead of `None`. Partial content is discarded and the agent loop's existing fallback-to-buffered path retries with a fresh non-streaming LLM call.

Missing coverage:
- There is no test for a stalled SSE stream, mid-tool-call timeout, or missing `[DONE]` / `message_stop`.

### 3. Medium - invalid `tools.sandbox_root` fails open to the current working directory — **FIXED (PR #158)**

Evidence:
- `crates/alms-runtime/src/agent.rs:99-115` canonicalizes `config.sandbox_root`.
- ~~If canonicalization fails, `crates/alms-runtime/src/agent.rs:103-112` falls back to `current_dir()` and continues with that as the active sandbox root.~~

Impact:
- A typo or bad deployment path can silently widen the accessible filesystem from the intended restricted directory to the process cwd.
- This is especially risky because the warning is easy to miss in production logs.

Safer behavior:
- ~~Fail closed on invalid sandbox roots, or require an explicit opt-in fallback.~~

**Resolution:** `AgentRuntime::new()` now returns `Result` and rejects unresolvable `sandbox_root` with `AlmsError::InvalidConfig`. Set `sandbox_root = ""` to explicitly opt out of sandboxing.

### 4. Medium - provider API key selection ignores the chosen provider and simply lets the last env var win

Evidence:
- `crates/alms-core/src/config.rs:100-108` applies `OPENROUTER_API_KEY`, then `OPENAI_API_KEY`, then `ANTHROPIC_API_KEY`, all into the same field.
- `crates/alms-runtime/src/llm_types.rs:303-310` repeats the same precedence in the runtime-only env loader.

Impact:
- In a shell environment that exports multiple provider keys, the selected provider can end up using the wrong credential.
- Example: `ALMS_LLM_PROVIDER=openai` with both `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` set will end up using the Anthropic key because it is loaded last.

Missing coverage:
- I found no test that exercises multiple API-key env vars together with different `ALMS_LLM_PROVIDER` values.

### 5. Medium - much of the unified config surface is dead or only partially wired

Evidence:
- `crates/alms-gateway/src/gateway.rs:67-83` ignores the entire `[session]` block and uses `SessionConfig::default()` instead.
- `crates/alms-core/src/config.rs:247-249` defines `llm.max_retries` and `llm.max_tokens_per_run`, but repo-wide search found no production consumers.
- `crates/alms-session/src/types.rs:103-113` defines `auto_archive`, `archive_ttl_secs`, `max_messages`, and `max_context_tokens`.
- `crates/alms-session/src/lib.rs:138-160` appends messages without checking any session limits.
- `crates/alms-session/src/lib.rs:229-248` uses `idle_timeout_secs` inside `archive_idle()`, but it does not consult `auto_archive`.
- Repo-wide search found no production call site for `archive_idle()` outside tests.

Impact:
- `alms.toml` suggests operators can tune retention, context growth, archival, retries, and token caps, but most of those controls currently do nothing.
- This is likely to create false confidence in production limits and lifecycle behavior.

Missing coverage:
- No tests prove that session limits, archival flags, retry counts, or per-run token caps actually affect runtime behavior.

### 6. Medium - documentation entrypoints are stale or broken, and `docs/api.md` still disagrees with the server despite being updated today

Evidence:
- `docs/index.md:19-29` and `docs/index.md:40-42` link to files that do not exist:
  - `docs/capability-model.md`
  - `docs/policy-reasons.md`
  - `docs/approvals-ux.md`
  - `docs/artifacts.md`
  - `docs/dev-onboarding.md`
- `docs/TASKS.md:5-6` still claims that this docs spine is in place.
- `docs/api.md:123-161` documents `POST /sessions:resolve` and `GET /sessions/{session_id}`.
- `crates/alms-gateway/src/server.rs:341-343` actually exposes `POST /sessions`, `GET /sessions/{session_id}/messages`, and `GET /sessions/{agent_id}/{context_id}`.
- `crates/alms-gateway/src/server.rs:429-436` confirms that the implemented GET route resolves by `(agent_id, context_id)` and creates the session if missing.
- `README.md:23-33` is mojibaked in the project tree block and still omits `alms-coordinator`.
- `README.md:50-52` still describes "Append-only log + snapshots" and "WebSocket only", which no longer matches the current SQLite + SSE implementation.

Impact:
- New contributors will start from a broken docs index.
- Client implementers can build against the wrong session routes even if they consult the same-day API doc.
- The README no longer describes the actual storage or transport model on `main`.

## What looked healthy

- The workspace is green under `cargo test`.
- The workspace is clean under `cargo clippy --all-targets --all-features -- -D warnings`.
- Recent Anthropic/tool-call-grouping work is covered by focused unit tests in `crates/alms-runtime/src/anthropic.rs` and `crates/alms-runtime/src/context.rs`.
- Recent SQLite fixes around default-agent transactions and delete flows are covered by unit tests in `crates/alms-session/src/sqlite.rs`.

## Recommended next actions

1. Enforce `[tools].enabled` at runtime and reflect the real tool set in `/settings`.
2. ~~Change stream-stall handling from silent EOF to an explicit runtime error.~~ **Done (PR #175)**
3. ~~Make invalid `sandbox_root` fail closed.~~ **Done (PR #158)**
4. Resolve API-key loading by provider, not by env-var order.
5. Either wire the remaining config knobs into production code or remove them from the public config surface.
6. Repair `docs/index.md`, update `docs/api.md` session routes, and refresh `README.md` to match SQLite/SSE/current crate layout.

## Coverage gaps worth adding tests for

- Config-driven tool allowlisting.
- ~~Invalid `sandbox_root` behavior.~~ **Covered (PR #158)**
- Multi-provider API-key env precedence.
- ~~Stalled streaming responses.~~ **Covered (PR #175)** — error path tested implicitly via fallback; explicit timeout test recommended.
- Session archival and retention behavior.
- `/settings` staying aligned with the actual tool registry.
