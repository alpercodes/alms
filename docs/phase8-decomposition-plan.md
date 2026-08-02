# Phase 8 — Ownership-boundary decomposition

Status: implementation in progress on `codex/phase8-decomposition`.

Parent plan: [`engineering-stabilization-plan.md`](engineering-stabilization-plan.md)

## Objective

Reduce the largest remaining coordination modules only where Phases 1–7 have
already established a stable ownership boundary. This phase is a mechanical
decomposition, plus one independently tested notification-persistence fix. It
must not change the HTTP/SSE contract, database schema, lifecycle semantics,
frontend behavior, or crate dependency direction.

## Invariants

1. Run and job state still changes only through the lifecycle APIs established
   in Phase 3; moving code does not create a persistence or status bypass.
2. Durable state is committed before its in-memory projection or observable
   event, including notification input consumed by an agent run.
3. Run configuration has one resolver and the read API has no authority to
   mutate run state.
4. Runtime environment construction is deterministic and shared by top-level
   and subagent paths where their policies are meant to match.
5. Existing HTTP routes and wire behavior remain unchanged. The intentional
   public Rust API cleanup is limited to removing the unused coordinator
   constructor identity, making the internal running-subagent handle private,
   and replacing `AgentRuntime::tools()` with the narrower
   `AgentRuntime::register_tool()` mutation API. The project is not deployed,
   so these unused broad implementation surfaces do not receive compatibility
   shims. Newly extracted modules are private or crate-visible by default.
6. The nine-crate production dependency graph remains acyclic and gains no new
   normal dependency edge.

## Delivery

### Gateway configuration and read API extraction

- Move per-agent run configuration resolution and resolved-run snapshot
  construction out of `runs/mod.rs` into
  `configuration/resolution.rs`, behind the existing `configuration` module.
- Move read-only run handlers and their response/query types out of the core
  lifecycle implementation into `runs/read_api.rs`. This includes run listing,
  run status, persisted tool calls, in-flight reasoning, and in-flight text
  reads.
- Keep admission, cancellation, and execution behavior on their established
  paths. Route registration and the public HTTP handler/query re-exports remain
  stable; internal configuration resolver exports are deliberately narrowed.
- Keep test-only barriers and lifecycle regression hooks adjacent to the code
  whose ordering they control.

### Runtime environment extraction

- Move the `AgentRuntime` constructor and its order-sensitive filesystem,
  shell, workspace, spill-path, and tool-builder methods from
  `agent/mod.rs` into `agent/environment.rs`.
- Keep all of these methods as inherent `AgentRuntime` methods so callers and
  builder chaining remain unchanged; the extraction introduces no parallel
  environment object or gateway-owned setup path.
- Preserve builder ordering and re-registration behavior. In particular,
  workspace, shell defaults, permissions, filesystem roots, spill directories,
  and tool configuration must still produce the same final registry regardless
  of which supported builder order a caller uses.

### Coordinator and API cleanup

- Remove the unused coordinator `main_agent` field and constructor argument.
- Keep the running-subagent handle private and expose only the snapshots and
  operations required by callers.
- Narrow extracted helpers to the smallest visibility required by sibling
  modules and integration tests. Replace the broad public
  `AgentRuntime::tools()` registry accessor with `register_tool()`, which is the
  only operation production callers require. No compatibility shim is required
  because the project is not deployed, but other externally used gateway,
  runtime, and coordinator APIs must not disappear accidentally.

### Notification input persistence fix

This correctness change is reviewed and tested separately from the mechanical
moves. A user-facing system notification is part of the input to its run. If
persisting that input fails, execution must fail before calling the LLM rather
than logging the failure and continuing with history that cannot be recovered
after restart.

The regression test must inject an append failure and prove that:

- the LLM/runtime is not entered with an unpersisted notification;
- the run reaches the existing deterministic failure path;
- no successful completion event or reply is published; and
- restart-visible state does not claim that the missing input was processed.

## Visibility and dependency gates

- New modules are private; helpers use `pub(super)` or `pub(crate)` only when a
  real sibling or cross-crate caller requires it.
- The before/after set of normal workspace edges must remain:

  ```text
  alms-channel     -> alms-core
  alms-cli         -> alms-core, alms-gateway, alms-session
  alms-coordinator -> alms-core, alms-runtime, alms-session, alms-tools
  alms-gateway     -> alms-channel, alms-coordinator, alms-core,
                      alms-runtime, alms-session, alms-tools
  alms-runtime     -> alms-core, alms-sandbox, alms-session
  alms-sandbox     -> alms-core
  alms-session     -> alms-core
  alms-tools       -> alms-core, alms-sandbox, alms-session
  alms-core        -> none
  ```

- No new direct run/job persistence write or status mutation may appear
  outside the established lifecycle/store owners.
- Moving code must not alter serialized types, routes, event names, migrations,
  or generated frontend assets.

## Validation

Run the local build, test, audit, and browser gates that correspond to the CI
jobs:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo build --release

npm ci
npm run ui:audit
npm run ui:check
npm run ui:build
npm run ui:test:e2e
```

GitHub CI also runs `rustsec/audit-check` against `Cargo.lock` in its separate
security-audit job. Its frontend job installs Chromium with
`npx playwright install --with-deps chromium` before invoking the Playwright
script. Local environments may need `cargo audit` (the local RustSec
equivalent) and `npx playwright install chromium` once before running the
commands above; `--with-deps` is the Linux CI setup step rather than a
repository test command.

Additionally:

- run the focused gateway notification-persistence regression;
- run `cargo test -p alms-gateway`, `cargo test -p alms-runtime`, and
  `cargo test -p alms-coordinator` while iterating on their respective moves;
- compare `cargo metadata --no-deps --format-version 1` before and after;
- confirm `crates/alms-gateway/static/ui-dist/` has no generated drift; and
- check local Markdown links after moving completed phase documents.

## Rollback

Phase 8 has no schema migration or wire change. Reverting the Phase 8 commit
restores the prior module layout without a data downgrade. If the independent
notification-persistence fix must be diagnosed separately, revert or bisect
that commit without retaining a partially moved implementation. The existing
`v0.2.4-pre-stabilization` and `v0.2.4-pre-frontend-migration` tags remain
source checkpoints; neither tag is a database rollback mechanism.

## Non-goals

- No new crate, framework, service, database, transport, or frontend rewrite.
- No redesign of run, job, queue, SSE, or configuration semantics.
- No broad cleanup of every large source file or historical design document.
- No recursive subagent spawning, richer progress protocol, or
  `invoke_agent` result truncation.
- No compatibility layer for private implementation paths that have no caller.
