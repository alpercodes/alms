# Deferred Bugs & Known Issues

Bugs found during code reviews that were not fixed immediately. Tracked here so they don't get lost. Fix when the affected area is next touched, or when they become blocking.

**Format:** `[source] severity — description. Why deferred.`

---

## From review of #46 (Agent registry — data model) — 2026-03-12

- **[sqlite.rs] S3 Medium** — `delete_agent` orphans sessions and jobs. Neither `sessions.agent_id` nor `jobs.agent_id` has a FK constraint to `agents.id`. Deleting an agent leaves orphaned rows. *Deferred: design decision — need to decide cascade behavior.*

- **[sqlite.rs] S4 Low** — `touch_agent` silently succeeds on nonexistent agent (doesn't check affected row count). Fire-and-forget on hot path, so acceptable. *Deferred: defense-in-depth.*

- **[sqlite.rs] S4 Low** — No index on `agents.is_default`. Not needed at MVP scale. *Deferred: performance improvement if agent count grows.*

- **[sqlite.rs] S4 Low** — `set_default_agent` uses manual `BEGIN`/`COMMIT`/`ROLLBACK` instead of `conn.transaction()`. Current code is correct and tested (including rollback on nonexistent ID). *Deferred: style improvement.*

## From review of #47 (Agent auto-migration) — 2026-03-12

- **[gateway.rs] S3 Medium** — Migration hardcodes `name: "main"` without calling `validate_agent_name`. Safe today since "main" passes validation. If "main" is ever reserved, migration would insert an invalid name. *Deferred: add validation call if reserved list expands.*

- **[gateway.rs] S4 Low** — TOCTOU race between `list_agents().is_empty()` and `create_agent()` — two concurrent callers could both see empty and attempt INSERT. Harmless in single-process daemon; `create_agent` fails on UNIQUE/PK constraint, caught by `warn!`. *Deferred: single-process only.*

- **[gateway.rs] S4 Low** — `is_default: true` set via INSERT without going through `set_default_agent()`. Correct since table is empty (no existing defaults to clear). *Deferred: no action unless empty-table guard is relaxed.*

- **[gateway.rs] Nit** — Missing `#[instrument]` on `migrate_sidecar_agent`. *Deferred: add next time file is touched.*

## From review of #48 (Agent HTTP API) — 2026-03-12

- **[agents.rs] S2 Medium** — Migration comment in `gateway.rs` is misleading about why removing `set_default_agent()` is safe. The real reason is the `agents.is_empty()` early-return guard, not because `create_agent` handles default clearing. *Deferred: comment-only fix, low risk.*

- ~~**[agents.rs] S4 Low** — `update_agent` SQL overwrites `name` field from the existing record.~~ **Fixed** in P10 review fixes commit.

- ~~**[sqlite.rs] S5 Medium** — `list_agents` silently drops rows with parse errors.~~ **Fixed** in P10 review fixes commit.

- **[settings.rs] S6 Medium** — `GET /settings` double-swallows errors when listing agents (`.ok()` + `.unwrap_or_default()`). Returns empty agents array even if DB is corrupted. *Deferred: settings is a convenience endpoint, not critical path.*

- ~~**[sqlite.rs] S9 Low** — `update_agent` doesn't check affected row count.~~ **Fixed** in P10 review fixes commit (now returns `AgentNotFound`).

- **[agents.rs] S10 Low** — No integration tests for HTTP handlers. Only `resolve_agent` and store-layer unit tests exist. *Deferred: needs test infrastructure (TestClient setup).*

- **[agents.rs] S11 Low** — Duplicate name detection relies on error string matching (`msg.contains("UNIQUE")`). Fragile across SQLite versions. Should match on `rusqlite::ErrorCode::ConstraintViolation`. *Deferred: works today, needs AlmsError refactor to expose rusqlite error codes.*

- **[agents.rs] S12 Low** — Repetitive error-mapping boilerplate (~10 sites mapping to `(StatusCode, Json<Value>)`). Could extract `internal_error()`, `not_found()`, `conflict()` helpers. *Deferred: cleanup task.*

## From review of #51 (CLI session commands) — 2026-03-12

All findings were fixed.

## From review of #49 (Per-agent config overrides) — 2026-03-12

- **[runs.rs] S3 Medium** — Legacy `run_agent` endpoint (`POST /agent/run`) bypasses per-agent config overrides entirely. It calls `execute_run` with `RunOverrides::default()`, which is correct, but the endpoint doesn't go through the same session/agent resolution as the canonical `/runs` path. Pre-existing issue, predates task #49. *Deferred: legacy endpoint — deprecate or remove.*

- **[runs.rs] S3 Medium** — Zero test coverage for the three-layer config merging logic. Should extract into a pure function `merge_agent_config(base, agent_record, overrides)` and add unit tests. *Deferred: needs refactor to make testable.*

- **[runs.rs] S4 Low** — `system_prompt` per-agent override is a full replacement, not an append. If agent has a system_prompt override, the server default system prompt is entirely replaced. This is by design but not documented. *Deferred: document behavior.*

- **[runs.rs] S4 Low** — Per-agent `temperature` and `max_tokens` overrides are not supported — only per-run overrides exist. Adding them to `AgentRecord` would require schema migration. *Deferred: future enhancement.*

- **[runs.rs] S4 Low** — Posture string parsing (`"guarded"` / `"full_control"`) is duplicated between `runs.rs` (lines 197-204, 223-234) and `agents.rs` (validate_posture). Should extract a shared `Posture::from_str` impl. *Deferred: cleanup task.*

## From review of #52 (CLI run/job commands) — 2026-03-12

- **[main.rs] S3 Low** — `api_client()` allocates a new `reqwest::Client` per HTTP call. Harmless for single-shot CLI invocations but wasteful if CLI ever does batch ops. *Deferred: no impact today.*

- **[main.rs] S7 Low** — `job_create` mixes direct SQLite access (to resolve agent) with HTTP API (to create job). Dual-access requirement; if CLI and gateway disagree on state, confusing errors. Would require gateway API change to accept agent name in job creation. *Deferred: design-level change.*

## From review of #53 (CLI dashboard/completions/health) — 2026-03-12

- **[main.rs] S3 Medium** — `process::exit(1)` in health error paths skips Rust destructors. Acceptable in current context (no cleanup needed), but fragile if cleanup is added later. *Deferred: no current impact.*

- **[main.rs] S6 Medium** — No tests for `health --json` — the headline feature of the commit. Requires mock HTTP server infrastructure. *Deferred: needs test infrastructure.*

- **[main.rs] S9 Low** — `open::that()` may silently succeed on headless systems (no browser). Known limitation of the `open` crate. *Deferred: can't fix without platform detection.*

## From review of #55 (LLM streaming hang fix) — 2026-03-12

- **[llm_client.rs] S1 Low** — 60s per-chunk read timeout is hardcoded, not configurable. Slow reasoning models or high-latency connections could hit this. Most providers send heartbeat comments during processing so 60s inter-chunk is generous. *Deferred: make configurable when config is next touched.*

- **[llm_client.rs] S2 Low** — Timeout terminates the stream silently (`None`), indistinguishable from normal completion to the caller. Partial content/tool_calls are returned as if complete. The `warn!` log helps debugging, and downstream JSON parse failures on truncated tool args provide a safety net. *Deferred: acceptable failure mode for now.*

## From bug review of 2026-03-08 (pre-existing, tracked in bug-review-2026-03-08.md)

All critical and medium bugs from that review were fixed in commit 437de9b. Remaining:

- **[agent.rs] Low** — Error message "No response from LLM" fires when `choices` is empty. Should say "LLM returned empty choices array". *Deferred: cosmetic.*

## From CLAUDE.md Known Issues (pre-existing)

- **4 sandbox/wasmtime tests fail** — "must use async instantiation when async support is enabled". Pre-existing wasmtime config issue.

- **shell_exec not truly sandboxed** — `shell_exec` cwd is restricted but the executed command can access files outside sandbox. Needs Landlock or restricted OS user for true isolation.

- **Guarded posture + parallel tool calls** — All approval requests fire simultaneously via `join_all` rather than sequentially. UX issue when LLM issues multiple tool calls.

## From review of #54 (UI agent selector & management) — 2026-03-12

- **[index.html] S2 Medium** — Session filtering uses client-side `===` comparison between `s.agent_id` (serde-serialized) and `S.activeAgentId` (from settings endpoint). Works today, but fragile if UUID serialization format changes. Should add `?agent_id=` query param to `GET /sessions` for server-side filtering. *Deferred: works correctly today, needs server-side change.*

- **[index.html] S3 Medium** — `checkBootstrapStatus()` fires N sequential requests (one per agent) to check workspace files. Slow at 10+ agents. Should use `Promise.allSettled()`. *Deferred: acceptable at current scale (1-3 agents).*

- **[index.html] S3 Medium** — `refreshAgents()` calls render functions, then `checkBootstrapStatus()` calls them again — double render on every agent CRUD operation. *Deferred: cosmetic.*

- **[index.html] S3 Low** — No client-side agent name format validation. Users typing "My Agent" get a server error (which is displayed). Could add `pattern` attribute to input. *Deferred: server error message surfaces correctly.*

- **[index.html] S4 Low** — `newSession()` uses `'web-chat-' + Date.now()` as context_id. Shows raw timestamp in sidebar (e.g. "web-chat-1741785123456"). *Deferred: functional, not pretty.*

- **[index.html] S4 Low** — Delete and set-default agent buttons silently swallow network errors (catch blocks are empty). *Deferred: edge case.*

---

# Accepted Findings

Observations noted during reviews that are not bugs — correct by design, cosmetic only, or explicitly out of scope. No action needed. Recorded for completeness.

## From review of #48 (Agent HTTP API)

- **S7 Cosmetic** — `create_agent` response doesn't reflect that the old default agent's `is_default` was cleared. Clients caching old records will have stale data. Not a server-side bug; `GET /agents` returns correct state.

- **S13 Cosmetic** — `llm_client.rs` change in this commit is unrelated reformatting (`cargo fmt` output). Should ideally be a separate commit.

- **S14 Cosmetic** — Commit includes unrelated P11/P12 task planning in TASKS.md. Muddies the diff scope but no code impact.

## From review of #51 (CLI session commands)

- **S7 Cosmetic** — `Commands::Sessions` renamed to `Commands::Session` (singular). Breaking CLI change, but we haven't shipped, so fine.

## From review of #52 (CLI run/job commands)

- **S3 Low** — New `reqwest::Client` allocated per HTTP call. Harmless for single-shot CLI. No TLS/pool reuse, but CLI makes 1 request per invocation.

- **S11 Cosmetic** — Unused `_uuid` binding in `job_cancel`. Was fixed during review.

- **S12 None** — CLAUDE.md and TASKS.md updates are accurate.

## From review of #53 (CLI dashboard/completions/health)

- **S2 Low** — Health `--json` success response shape differed from error shape. Was fixed during review (added `"ok"` envelope).

- **S4 Low** — Dashboard prints "Opening {url} ..." to stdout unconditionally. Standard UX for "open in browser" commands.

- **S5 Low** — Dashboard and Completions have no `--json` flag. Correctly scoped — Dashboard is a side-effect command, Completions outputs shell script.

- **S7 Low** — No test for Dashboard command. Side-effect heavy (`open::that()`), impractical to unit test.

- **S8 Cosmetic** — Completion tests are shallow smoke tests (`!buf.is_empty()`). Adequate for confirming `clap_complete` integration.

- **S10 Low** — `open = "5"` unpinned at major version. Standard Cargo semver, `Cargo.lock` pins to 5.3.3.

## From review of #55 (LLM streaming hang fix)

- **S3 Cosmetic** — `SseParseResult` is private but used in tests. Tests are in same module, so private access is correct Rust.

- **S4 None** — CRLF normalization (`replace("\r\n", "\n")`) is correct. Handles proxy servers that inject CRLF.

- **S5 None** — Remaining buffer parsing on stream end is correct. Handles servers that don't send trailing `\n\n`.

- **S6 Low** — `String::from_utf8_lossy` replaces invalid bytes with U+FFFD. Would corrupt JSON and cause downstream parse failure, which is the right failure mode.

- **S7 None** — Test coverage adequate: 5 parse tests + 1 mock stream integration test.

## From review of #54 (UI agent selector & management)

- **S4 Nit** — Duplicate `!agent.is_default` guard for set-default and delete buttons. Could consolidate into single block. Style preference only.

- **S4 Nit** — Missing `aria-label` on agent selector `<select>`. Has `title` attribute but not fully accessible. Accessibility improvement, no functional impact.

## From review of #46 (Agent registry — data model)

- **Convention** — 9 store tests + 8 validation tests. Coverage is adequate for CRUD operations and name validation.

## From review of #47 (Agent auto-migration)

- **None** — Migration is truly idempotent. Sidecar file reading handles missing/corrupt/permission cases. No race between migration and first API request (migration runs synchronously in `Gateway::new()` before HTTP listener starts). `store()` accessor returning `Option<&Arc<SqliteStore>>` is fine — callers don't clone the Arc.

## From review of #49 (Per-agent config overrides)

- **Info** — `touch_agent` is correctly placed after both success and failure paths in `execute_run`. Runs regardless of outcome.

- **Info** — Concurrent `touch_agent` vs `update_agent` is safe — SQLite mutex serializes access, and both set `last_active` to "now" so timestamps are within milliseconds.

- **Info** — `fire_job_run` and `stream_run_legacy` pass `RunOverrides::default()`, meaning no per-run overrides. Correct — jobs use agent config, legacy has no override fields.
