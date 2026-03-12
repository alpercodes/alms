# Deferred Bugs & Known Issues

Bugs found during code reviews that were not fixed immediately. Tracked here so they don't get lost. Fix when the affected area is next touched, or when they become blocking.

**Format:** `[source] severity — description. Why deferred.`

---

## From review of #48 (Agent HTTP API) — 2026-03-12

- **[agents.rs] S2 Medium** — Migration comment in `gateway.rs` is misleading about why removing `set_default_agent()` is safe. The real reason is the `agents.is_empty()` early-return guard, not because `create_agent` handles default clearing. *Deferred: comment-only fix, low risk.*

- **[agents.rs] S4 Low** — `update_agent` SQL overwrites `name` field from the existing record. Safe today since `UpdateAgentRequest` has no `name` field, but fragile if name is added later without validation. *Deferred: hypothetical future issue.*

- **[sqlite.rs] S5 Medium** — `list_agents` silently drops rows with parse errors via `filter_map(|r| r.ok())`. Same pattern we fixed in sessions/jobs but not yet applied to agents. *Fix next time agents code is touched.*

- **[settings.rs] S6 Medium** — `GET /settings` double-swallows errors when listing agents (`.ok()` + `.unwrap_or_default()`). Returns empty agents array even if DB is corrupted. *Deferred: settings is a convenience endpoint, not critical path.*

- **[sqlite.rs] S9 Low** — `update_agent` doesn't check affected row count. If agent ID doesn't match, silently succeeds. Callers verify existence first, so no current bug. *Deferred: defense-in-depth improvement.*

- **[agents.rs] S10 Low** — No integration tests for HTTP handlers. Only `resolve_agent` and store-layer unit tests exist. *Deferred: needs test infrastructure (TestClient setup).*

- **[agents.rs] S11 Low** — Duplicate name detection relies on error string matching (`msg.contains("UNIQUE")`). Fragile across SQLite versions. Should match on `rusqlite::ErrorCode::ConstraintViolation`. *Deferred: works today, needs AlmsError refactor to expose rusqlite error codes.*

- **[agents.rs] S12 Low** — Repetitive error-mapping boilerplate (~10 sites mapping to `(StatusCode, Json<Value>)`). Could extract `internal_error()`, `not_found()`, `conflict()` helpers. *Deferred: cleanup task.*

## From review of #51 (CLI session commands) — 2026-03-12

All findings were fixed.

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
