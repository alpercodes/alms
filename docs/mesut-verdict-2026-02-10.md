# Mesut review + sanity checks (2026-02-10)

This document captures my (Mesut) first pass through the ALMS workspace and the concrete “code sanity check” findings so other agents can pick up the same thread.

> Repo path reviewed: `</srv/alms`

## 0) Context / intent

- ALMS aims to be a technically superior, more usable, more secure alternative to OpenClaw.
- The docs/research emphasize coordinator-based multi-agent orchestration, explicit task boundaries, parallel execution, and capability/sandboxing.

## 1) High-level verdict (first pass)

### Strengths
- **Architecture direction is right**: coordinator-based hub-and-spoke, explicit task boundaries, “parallel by default”, security-first.
- **Research is valuable**: `research/session-issues.md` turns “OpenClaw is bad” into specific, actionable failure modes (locking races, lost updates, deadlocks, unbounded queues).
- **MVP skeleton exists**: Gateway/Runtime/Session/Channel/WASM sandbox all have scaffolding.

### Main risks
- There are **compile-time / wiring-level inconsistencies** that will block an end-to-end build/run soon.
- Several components are **placeholder implementations** (coordinator task execution, WASM sandbox memory protocol, etc.), which is fine, but needs explicit tracking.

## 2) Code sanity checks (from earlier copy + canonical repo)

### 2.1 Environment sanity check
- On this machine/session, `cargo` was not available (`cargo: command not found`). I therefore used static inspection instead of compiling.

### 2.2 Cargo/workspace dependency issues

#### A) Circular dependency (likely hard build break)
From crate manifests observed during review:
- `crates/alms-coordinator` depends on `crates/alms-runtime`
- `crates/alms-runtime` depends on `crates/alms-coordinator`

This is a **Cargo cycle** and normally will not compile. Fix by moving shared types/traits/messages into `alms-core` (or a new `alms-protocol` crate) so dependencies become one-way.

#### B) Duplicate dev-dependency entry
- In one copy seen during review, `crates/alms-runtime/Cargo.toml` had duplicated `[dev-dependencies] tokio-test.workspace = true`.

(If this was only in the copied folder, ignore; check canonical manifests.)

### 2.3 API/constructor signature mismatch (likely compile error)
In `crates/alms-gateway/src/server.rs`, `run_agent` tries to construct an `AgentRuntime` with a `SessionManager` where an `LlmClient` is expected.

- `alms-runtime::AgentRuntime::new(agent_id, config, llm: LlmClient)`
- `server.rs` appears to pass `gateway.session_manager().clone()` as the third argument.

This will not compile as-is; the handler likely needs access to `gateway.llm` or use a `RuntimeManager`.

### 2.4 Placeholder / incomplete implementations that need tracking

#### A) Coordinator / subagents
- `alms-coordinator` currently simulates work and returns JSON results; not executing real subagent loops yet.

#### B) Capability system duplication
- `alms-core::Capability` is an enum.
- `alms-coordinator::SubagentRequest.capabilities` is `Vec<String>`.

This is two capability systems already; unify early to avoid drift.

#### C) WASM sandbox memory protocol / allocation
In `crates/alms-sandbox/src/sandbox.rs`:
- `allocate()` effectively returns pointer `0` (no allocator / no tracking).
- Input is written at ptr 0.
- Result reading assumes a custom protocol: first 4 bytes = len, then bytes.

This is not safe/real yet; treat as placeholder until a real ABI and allocator strategy exists.

#### D) Session lookup inefficiency (MVP OK)
In `crates/alms-session`, `SessionManager::get(session_id)` scans all sessions to find a matching `SessionId`.

Fine for MVP, but will need indexing by `SessionId` once sessions grow.

## 3) Suggested next actions (short list)

1. **Break the Cargo dependency cycle** between runtime and coordinator.
2. **Make gateway compile**: fix the `AgentRuntime::new` wiring in HTTP handler.
3. **Decide capability representation** (enum vs string) and standardize.
4. **Define tool ABI for WASM** (memory layout, allocation, result passing) and implement a real allocator protocol.
5. Add a build/test CI check once tooling is in place.

## 4) One-by-one: alms-gateway (detailed)

Files:
- `crates/alms-gateway/src/lib.rs`
- `crates/alms-gateway/src/gateway.rs`
- `crates/alms-gateway/src/server.rs`
- `crates/alms-gateway/Cargo.toml`

### What it currently is
- There are **two concepts conflated** under “gateway”:
  1) A **channel router + agent runtime loop** (`gateway.rs::Gateway::run`) that polls Telegram and calls `AgentRuntime::run()`.
  2) An **HTTP API server** (`server.rs`) that exposes `/health`, `/sessions`, `/agent/run`, `/ws`.

That split is fine, but the integration points are currently inconsistent.

### Build-blocking inconsistencies (likely compile errors)

1) **`serve()` signature mismatch (CLI vs server)**
- `alms-cli` calls: `alms_gateway::serve(&bind).await?;`
- `alms-gateway::server::serve` is defined as:
  - `pub async fn serve(bind_addr: &str, gateway: Gateway) -> AlmsResult<()>`

So the CLI call cannot compile unless there’s another overload (there isn’t).

2) **`AgentRuntime::new` called with wrong argument type in HTTP handler**
In `server.rs::run_agent`, it does:
- `AgentRuntime::new(..., gateway.session_manager().clone())`

But `AgentRuntime::new` (in `alms-runtime`) expects a third argument of type `LlmClient`, not a `SessionManager`.

3) **Unused/incorrect imports**
- `server.rs` imports `GatewayConfig`, `Session`, `SessionConfig` but doesn’t use them.

### Runtime/architecture issues to decide

- **Single source of truth for message processing**: Right now:
  - `gateway.rs::Gateway::run()` contains a message loop for Telegram polling and agent invocation.
  - `server.rs::serve()` just starts axum and does not call `Gateway::initialize_channels/start/run`.

So “starting gateway” via HTTP server won’t actually process Telegram messages, and “starting gateway” via `Gateway::run()` won’t expose HTTP.

**Suggestion:** pick one of these patterns:

A) *HTTP server owns the Gateway* (recommended for MVP)
- `serve(bind, Gateway)` should:
  - call `gateway.initialize_channels().await?`
  - call `gateway.start().await?`
  - spawn `gateway.run().await` in a background task
  - run axum server

B) *Gateway owns the HTTP server*
- `Gateway::start()` spawns both axum and channel loops.

Either is fine, but make it explicit.

### Design notes
- The `Gateway` struct already has `llm: LlmClient` and `session_manager`, so HTTP endpoints should not be reconstructing runtimes incorrectly. If you want per-request runtimes, introduce a `RuntimeManager` (runtime factory) inside gateway.

## 4.1) One-by-one: alms-cli

File: `crates/alms-cli/src/main.rs`

### Findings
- CLI `Gateway` subcommand calls `alms_gateway::serve(&bind)`.
- This currently conflicts with `alms-gateway/server.rs::serve(bind, gateway)` (needs a `Gateway` instance). This is a **build blocker** until the startup story is unified.

## 4.2) One-by-one: alms-channel (Telegram)

Files:
- `crates/alms-channel/src/lib.rs`
- `crates/alms-channel/src/telegram/mod.rs`
- `crates/alms-channel/src/telegram/types.rs`

### Findings
- MVP polling adapter is reasonable (converts Telegram updates → `alms-core::IncomingMessage`, parses `/commands`).
- Some dead code / unused fields exist (`base_url` is stored; `Url` import exists; etc.) but not harmful.
- Webhook support is sketched but not end-to-end (there’s no HTTP webhook receiver in `alms-gateway` yet).

### Suggestion
- Decide one update ingestion mechanism for MVP:
  - polling only (simplest), OR
  - webhook only (requires public URL + verification + secret token checking).

## 4.3) One-by-one: alms-runtime

Files:
- `crates/alms-runtime/src/agent.rs`
- `crates/alms-runtime/src/llm_client.rs`
- `crates/alms-runtime/src/llm_types.rs`
- `crates/alms-runtime/src/tools.rs`
- `crates/alms-runtime/src/main_agent.rs`

### Findings
- `AgentRuntime` implements a classic tool-call loop against an OpenAI-style `/chat/completions` API.
- `tools.rs` defines a **separate** tool system (native Rust tools + JSON schema) that is not integrated with `alms-sandbox`.
- `main_agent.rs` is an orchestration sketch that depends on `alms-coordinator` (reinforces the dependency cycle).

### Issues / risks
- There are now **two tool registries**:
  - `alms-runtime::ToolRegistry` (native tools, OpenAI tools schema)
  - `alms-sandbox::ToolRegistry` (native + WASM tools)

This duplication will cause drift. Pick one.

### Suggestions
- For MVP: unify runtime tool execution to one registry. If you want WASM sandbox as the security boundary, the runtime should call into `alms-sandbox` for all tool execution.
- `MainAgent` should probably live in coordinator (or vice versa), but the crate boundaries need to be redrawn to avoid cycles. A good split:
  - `alms-core` (types/protocol)
  - `alms-runtime` (LLM client + single-agent loop)
  - `alms-coordinator` (multi-agent orchestration, depends on runtime)

## 4.4) One-by-one: alms-session store

File: `crates/alms-session/src/store.rs`

### Findings
- `MemoryStore` + optional snapshot JSON is a decent MVP persistence layer.
- Potential concern: it uses `AtomicBool loaded` to avoid multiple loads, but does not guard concurrent first-use loads beyond that flag; still probably OK given usage.

### Suggestion
- If you want to avoid OpenClaw’s file-locking pitfalls, consider adopting an append-only log (as docs suggest) or SQLite early.

## 4.5) One-by-one: alms-sandbox

Files:
- `crates/alms-sandbox/src/lib.rs`
- `crates/alms-sandbox/src/registry.rs`
- `crates/alms-sandbox/src/builtin.rs`
- `crates/alms-sandbox/src/error.rs`
- `crates/alms-sandbox/src/sandbox.rs`

### Findings
- Registry design (DashMap + Arc tools) is solid for concurrency.
- Built-in tools are more complete here than runtime’s builtins (math has actual operations).
- The sandbox execution is **not ABI-stable yet** (see earlier notes): allocation returns ptr=0, result protocol assumptions, timeout enforcement is checked after completion, etc.

### Suggestion
- Treat sandbox as “prototype” until the tool ABI is defined and enforced.

## 5) Files referenced
- `README.md`
- `docs/architecture.md`
- `research/session-issues.md`
- `crates/alms-core/*`
- `crates/alms-session/*`
- `crates/alms-runtime/*`
- `crates/alms-coordinator/src/lib.rs`
- `crates/alms-gateway/src/server.rs`
- `crates/alms-gateway/src/gateway.rs`
- `crates/alms-gateway/src/lib.rs`
- `crates/alms-cli/src/main.rs`
- `crates/alms-channel/src/telegram/mod.rs`
- `crates/alms-channel/src/telegram/types.rs`
- `crates/alms-sandbox/src/sandbox.rs`
- `crates/alms-sandbox/src/registry.rs`
- `crates/alms-sandbox/src/builtin.rs`
- `crates/alms-sandbox/src/error.rs`
