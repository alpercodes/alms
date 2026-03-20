# ALMS Product Review — March 2026

## Context

This is a comprehensive product review of ALMS at its current state (2026-03-20). The review covers bugs and issues found in the codebase, evaluates the planned user experience direction, and rates the product's readiness across key dimensions. No code changes are included — this is a read-only assessment.

---

## Executive Summary

ALMS is a technically strong, well-architected Rust system with ~175+ tests, clean module boundaries, and solid foundational abstractions. The core agent loop, SSE streaming, SQLite persistence, multi-agent hierarchy, and CLI tooling are all functional and well-tested.

However, **the product is not yet aligned with its own UX vision**. The `ux-principles.md` document describes an opinionated "agent operating system" — but the current implementation delivers closer to "chat with tools and a run timeline." Several critical UX gaps, unfinished features, and correctness bugs need resolution before the product can deliver on its promised experience.

**Overall rating: 6.5/10** — Strong foundation, incomplete product surface.

---

## Part 1: Bugs & Issues Found

### CRITICAL (data loss, security, or broken core flows)

#### BUG-1: Shell sandbox escape (known, unfixed)
- **File**: `crates/alms-sandbox/src/builtin.rs` (shell_exec)
- `shell_exec` restricts `cwd` but the executed command itself can access any file on the system. An agent can `cat /etc/shadow`, read SSH keys, or exfiltrate secrets.
- **Documented** in CLAUDE.md and TASKS.md (#86) but unfixed. This is a deployment blocker for any public-facing instance.
- **Severity**: CRITICAL for production, acceptable for private dev.

#### BUG-2: Run cancellation doesn't propagate to subagents (known, unfixed)
- **File**: `crates/alms-coordinator/src/lib.rs`
- Cancelling a parent run drops the `join_all` future but subagent tasks continue running, burning LLM tokens until completion or timeout.
- **Impact**: Users cancel a run expecting to stop costs, but subagents keep spending.
- **Severity**: HIGH for cost management.

#### BUG-3: Telegram HTML parse_mode breaks most LLM responses (#61, unfixed)
- **File**: `crates/alms-channel/src/telegram/mod.rs`
- `sendMessage` uses `parse_mode: "HTML"` but LLM output regularly contains `<`, `>`, `&` which Telegram rejects as malformed HTML.
- **Impact**: Many agent responses silently fail to deliver. User sees nothing.
- **Severity**: CRITICAL for Telegram users.

#### BUG-4: Telegram polling latency — unnecessary 5s delay (#58, unfixed)
- **File**: `crates/alms-channel/src/telegram/mod.rs`
- `interval(5s)` wraps a 30s long-poll. After HTTP returns, the loop waits an extra 5s before re-polling. Messages sit undelivered during the gap.
- **Impact**: 5-second minimum latency on every Telegram message.
- **Severity**: HIGH for responsiveness.

#### BUG-5: Telegram 4096-char limit not handled (#60, unfixed)
- LLM responses exceeding 4096 chars are rejected by Telegram API. Error is logged but user gets no reply.
- **Severity**: HIGH for Telegram users.

#### BUG-6: `delete_agent` orphans sessions and jobs (#85, unfixed)
- Deleting an agent leaves sessions and jobs pointing at a nonexistent agent ID. No cascade.
- **Impact**: Orphaned data, broken queries, confusing UI state.
- **Severity**: HIGH for data integrity.

#### BUG-7: Config partially wired (#101)
- `AlmsConfig.session` is loaded but `gateway.rs` hardcodes `SessionConfig::default()`.
- `server.bind` is loaded but CLI startup ignores it unless `--bind` is passed.
- **Impact**: Users edit `alms.toml` expecting changes to take effect, but they don't. Silent misconfiguration.
- **Severity**: MEDIUM — confusing but not data-losing.

### HIGH (significant UX or correctness issues)

#### BUG-8: Web UI input disabled during active run
- Chat input is disabled while a run is executing. Users cannot type or queue follow-up messages.
- Backend `SessionQueue` already supports FIFO queuing but the UI doesn't use it.
- **Impact**: Major UX friction — feels like a hang. Users can't prepare next instruction while agent works.

#### BUG-9: No typing/thinking indicator between send and first token
- `run_started` SSE event is emitted but UI has no listener. Between send and first `token_delta`, the chat area is blank.
- **Impact**: User doesn't know if message was received or system is broken.

#### BUG-10: Subagent result truncation not implemented (#82)
- `invoke_agent` returns full subagent responses into parent context. With multi-level delegation, parent context bloats rapidly.
- **Impact**: Context window exhausted quickly; token costs inflate; agent loses track of its own conversation.

#### BUG-11: Empty message bubbles in web UI
- Token deltas create bubbles before text arrives, or tool-call-only responses create empty bubbles.
- **Impact**: Cluttered chat history, confusing visual noise.

#### BUG-12: Telegram per-agent config overrides ignored (#106)
- Telegram handler creates `AgentRuntime` with server defaults. Per-agent model/system_prompt/posture overrides are bypassed.
- HTTP run path correctly applies overrides but Telegram path doesn't.
- **Impact**: Switching default agent doesn't change model/prompt for Telegram.

#### BUG-13: `max_iterations` styled as normal text (#68)
- When `max_iterations` is reached, `"[Max iterations reached]"` appears as an ordinary agent message.
- **Impact**: User can't distinguish a normal response from a safety limit hit.

#### BUG-14: Approval denial has no reason field
- User clicks "Deny" on approval. Run fails with "approval_denied" but agent gets no reason.
- **Impact**: Agent can't learn why it was blocked; can't adjust approach.

### MEDIUM (quality, robustness, or minor UX gaps)

#### BUG-15: WASM sandbox `expect()` calls in production code
- `sandbox.rs:131` — `Engine::new(&wasm_config).expect("Failed to create WASM engine")` — panics on WASM initialization failure.
- `sandbox.rs:242,246` — `expect("memory export")`, `expect("read memory")` — panics on malformed WASM modules.
- **Impact**: Crafted WASM tool could crash the server process.

#### BUG-16: Config load errors are warnings, not fatal
- `AlmsConfig::load()` failure is caught with `unwrap_or_else` + `warn!` + default config.
- **Impact**: Typo in `alms.toml` → silently uses defaults. User thinks config is working.

#### BUG-17: SSE error mid-stream has no error event
- If SSE stream encounters error mid-run, it just closes. No `run_error` event emitted.
- Client reconnects and gets stale events via `Last-Event-ID`.

#### BUG-18: Token count config confusion (#113)
- `session.max_context_tokens` vs `context.max_input_tokens` — both default to 128k, serve different purposes, relationship undocumented.
- **Impact**: Users tune the wrong setting and get unexpected context truncation.

#### BUG-19: Env var naming inconsistency
- `ALMS_LLM_PROVIDER` vs `OPENROUTER_API_KEY` — mixed prefixes.
- `ALMS_DB_PATH` vs other config vars — naming not systematic.

#### BUG-20: Web UI error handling is opaque
- API errors from `apiFetch()` propagate but components don't catch/display them.
- **Impact**: Request failures result in blank UI with no feedback.

---

## Part 2: UX Vision vs. Reality

The `ux-principles.md` document (authored by Mesut, 2026-02-14) defines 9 principles. Here's how the current implementation scores against each:

| # | Principle | Score | Status |
|---|-----------|-------|--------|
| 0 | Core UX primitive is not chat | 4/10 | The UI IS chat. Session→Goals→Runs→Outcomes model exists in API but UI emphasizes the chat view, not the run timeline. |
| 1 | Run Timeline is primary UI | 6/10 | SSE events capture the timeline well. But UI doesn't show `status` events (building context, summarizing, calling LLM). Dead air between events. |
| 2 | Artifacts are currency | 1/10 | Not implemented at all. No artifact storage, no diff output, no artifact linking. Agents produce text, not reviewable artifacts. |
| 3 | Diff-first by default | 1/10 | Not implemented. No "propose before apply" UX. Agents write files directly via `fs_write`. No ChangeSet concept. |
| 4 | Tight feedback loops | 5/10 | CLI is tight (good commands, fast output). Web UI has latency gaps, missing status indicators, disabled input during runs. |
| 5 | Human decisions first-class | 7/10 | Approval UX exists and works. But approval card shows minimal info (no exact command/params). Deny has no reason field. |
| 6 | Cost + time visible | 4/10 | Token usage shown per-run. But no running cost estimate, no tool durations, no cumulative cost dashboard. |
| 7 | Team choreography | 1/10 | Not implemented. No agent assignment, no blocker visibility, no team view. Multi-agent exists but no coordination UX. |
| 8 | Spec is law | 0/10 | Not implemented. No spec linking, no drift detection, no policy gating. |

**UX Vision Alignment Score: 3.2/10**

The product has strong infrastructure for principles 1 and 5 but hasn't yet built the distinctive UX layers that would make ALMS feel like "operating an agent system" rather than "chatting with an LLM."

---

## Part 3: What's Working Well

### Architecture (9/10)
- Clean crate dependency graph with no cycles
- Newtype wrappers for all IDs (type-safe, no string confusion)
- `thiserror` for library errors, `anyhow` in binary — correct pattern
- `tracing` with structured fields throughout
- Session queue with per-session FIFO ordering — elegant concurrent design

### Agent Runtime (8/10)
- Context builder with 3 strategies (truncate, full, sliding-summary) is well-designed
- Workspace files (personality/goals/memories/user) are a strong UX concept
- Tool registry with JSON Schema parameters works correctly
- Bootstrap interview flow for new agents is a nice touch
- Parallel tool execution with `join_all` is correct

### Persistence (8/10)
- SQLite with WAL mode — good choice for single-process
- Write-through on every mutation — no lazy flush bugs
- Graceful shutdown flushes WAL properly
- Schema migration via `CREATE TABLE IF NOT EXISTS` — simple and effective

### SSE Streaming (8/10)
- Token-by-token streaming works correctly
- Multi-subscriber support with dead sender pruning
- Event replay via `Last-Event-ID` with dedup
- Proper TCP chunk boundary handling in SSE parser

### CLI (7/10)
- Comprehensive command set covering all operations
- Consistent `--json` flag across all commands
- Direct SQLite access (no gateway needed for read-only ops)
- Shell completions for bash/zsh/fish

### Test Coverage (7/10)
- 175+ tests across all crates
- Golden tests for SSE format
- Mock LLM for testing without API keys
- Deterministic scheduler tests with time mocking
- Good coverage of core paths; some gaps in edge cases

---

## Part 4: Product Direction Rating

### Where ALMS is heading (based on TASKS.md roadmap)

The roadmap is ambitious and well-prioritized. Key upcoming areas:

1. **Autonomous Subagents (P13)** — recursive spawning, progress reporting, cost budgets
2. **Agent Loop UX (P12)** — status events, crash safety, partial response recovery
3. **Web UI Polish (P15)** — addressed many UI gaps
4. **Telegram Rework (P11)** — fixing critical adapter bugs

### Strategic Gaps in the Roadmap

1. **No artifact system planned** — The most distinctive UX principle (artifacts as currency, diff-first) has no tasks in the roadmap. This is the single biggest gap between vision and plan.

2. **No cost/budget management** — Token usage is logged but there's no budget enforcement, cost estimation, or spend alerts. For autonomous agents burning API tokens, this is essential.

3. **No user authentication beyond bearer token** — Single shared token for all users. No per-user identity, no user-level permissions. Fine for personal use, blocker for team use.

4. **No webhook/callback support** — Only polling-based integrations (Telegram). No way to receive webhooks from external services or notify external systems on events.

5. **No plugin/extension system** — Tools are compiled into the binary. No way for users to add custom tools without modifying source code (beyond WASM, which has limited capabilities).

6. **No observability** — No metrics (Prometheus/OpenTelemetry), no distributed tracing for multi-agent flows, no alerting on error rates or token spend spikes.

### Product-Market Fit Assessment

ALMS occupies an interesting niche: a self-hosted, multi-agent system with persistence, workspace identity, and approval workflows. The closest competitors are:

- **Claude Code / Cursor** — developer-facing, single-agent, IDE-embedded
- **AutoGPT / CrewAI** — multi-agent but focused on task automation, no workspace identity
- **OpenAI Assistants API** — hosted, single-agent, no multi-agent hierarchy

ALMS differentiates on: self-hosted control, multi-agent hierarchy, agent workspace identity (personality/goals/memories), and approval workflows.

**But** the differentiation only works if the unique features are polished enough to use. Currently:
- Workspace identity is functional but has no guidance (what makes a good `personality.md`?)
- Multi-agent is functional but has no visibility (can't see what subagents are doing)
- Approvals work but are minimal (no details on what's being approved)

---

## Part 5: Ratings Summary

| Dimension | Rating | Notes |
|-----------|--------|-------|
| **Architecture** | 9/10 | Clean, modular, well-layered. No fundamental design flaws. |
| **Code Quality** | 8/10 | Good error handling, proper async patterns, comprehensive tests. A few `expect()` calls in prod paths. |
| **Core Agent Loop** | 8/10 | Streaming, tools, context management all work. Missing crash safety and partial recovery. |
| **Multi-Agent** | 6/10 | Hierarchy works. Missing: result truncation, progress reporting, recursive spawning, cancel propagation. |
| **Web UI** | 5/10 | Functional but rough. Disabled input during runs, no loading states, opaque errors, no keyboard shortcuts. |
| **CLI** | 7/10 | Solid command set. Missing: config validate, status overview, progress indicators. |
| **Telegram Adapter** | 4/10 | Multiple unfixed critical bugs: HTML parsing, 4096 limit, polling latency, per-agent config bypass. |
| **Security** | 5/10 | Bearer auth works. Sandbox has known escape. No per-user auth. Secrets handled correctly (env-only). |
| **Documentation** | 6/10 | CLAUDE.md and design docs are excellent. Missing: user guides, API formalization, workspace best practices. |
| **UX Vision Alignment** | 3/10 | Strong vision document, very little of it realized. Artifacts, diff-first, team choreography, specs all missing. |
| **Production Readiness** | 4/10 | Fine for personal/dev use. Not ready for public deployment (sandbox escape, no user auth, no observability). |

**Overall: 6.5/10** — Technically impressive foundation with significant product surface gaps.

---

## Part 6: Top 10 Recommendations (Priority Order)

1. **Fix Telegram HTML parse_mode** (#61) — 1 day, unblocks the Telegram channel entirely
2. **Enable chat input during active runs** — 1 day, biggest single UX improvement
3. **Add thinking/typing indicator** (#67) — 1 day, eliminates dead air perception
4. **Implement result truncation for subagents** (#82) — 2 days, prevents context bloat
5. **Wire config values that are loaded but ignored** (#101) — 1 day, prevents silent misconfiguration
6. **Design and implement artifact system** — multi-week, but this IS the product differentiator
7. **Add `fs_edit` search-and-replace tool** (#110) — 2 days, makes agents dramatically more useful for code tasks
8. **Propagate cancellation to subagents** — 2 days, prevents token waste
9. **Add Landlock or restricted user for shell_exec** (#86) — 3 days, deployment blocker
10. **Create getting-started guide and workspace templates** — 2 days, enables user adoption

---

*Review conducted 2026-03-20. Based on full codebase exploration of all 8 crates, 175+ tests, 14 documentation files, and TASKS.md roadmap analysis.*
