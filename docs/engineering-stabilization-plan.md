# ALMS Engineering Stabilization Plan

Status: approved for implementation

Baseline checkpoint: v0.2.4-pre-stabilization

Baseline commit: eadd4b3b664b8e263fd10470ee0b085303deb82c

This plan turns the architecture review into an ordered implementation program. The goal is not a rewrite. It is to make concurrency, persistence, event recovery, and client state explicit enough that the same classes of bugs stop recurring.

## Executive recommendation

Keep the Rust, Tokio, Axum, SQLite, and SSE backend. The current backend stack fits a single-daemon agent coordination system well. Most observed failures come from lifecycle invariants that are distributed across modules, not from the language or core libraries.

Stabilize backend lifecycle and persistence invariants before restructuring the frontend. A frontend rewrite performed first would still consume ambiguous run, queue, and replay semantics and would reproduce stale-state bugs in a cleaner codebase.

For the frontend, retain Preact but move to a typed build:

- Preact with TypeScript in strict mode
- Vite for build and development
- Zod at HTTP, SSE, and persisted-browser-state boundaries
- A normalized reducer/store organized around agents, sessions, runs, and jobs
- Vitest and Testing Library for unit and component coverage
- Playwright for reconnect and multi-session browser flows
- ESLint and Prettier with CI enforcement
- Node.js 22 pinned for reproducible builds
- Production assets embedded in the existing Rust binary

Do not adopt a second backend service, PostgreSQL, WebSockets, or a React framework during this program unless operating requirements change.

## Rollback contract

The tag v0.2.4-pre-stabilization protects the source tree and reproducible binary at the baseline commit. It does not roll back an already-migrated SQLite database.

All database changes in this program must therefore be additive and backward-aware:

- Never repurpose an existing enum value or column meaning.
- Add nullable columns or new tables before depending on them.
- Keep legacy readers functional during the compatibility window.
- Do not immediately persist new JobStatus strings that the tagged binary parses as Pending.
- Represent newly separated job terminal causes in additive fields, such as terminal_reason, until the old reader is retired.
- Back up the database before deploying a migration-bearing release.
- Document the minimum binary version that can read each schema revision.

Rollback verification must include both a fresh database and a copy of a migrated database that is still expected to be readable by the checkpoint binary.

## Program invariants

The following invariants are the acceptance criteria for the program:

1. At most one queue worker owns a given agent key at a time.
2. Admission is bounded per agent and globally, and rejection occurs before durable side effects.
3. A run can only move through valid, monotonic lifecycle transitions.
4. A stale persistence write cannot replace a newer run snapshot.
5. Browser state can always recover from an SSE replay gap or server restart.
6. Subscriber, queue, and event-log memory have explicit limits and prompt cleanup.
7. Frontend entities have one authoritative representation and derived views do not mutate it.
8. Database migrations are versioned, additive, tested, and compatible with the stated rollback window.
9. Every fixed concurrency bug receives a deterministic regression test at the invariant boundary.

## Phase 1 — Make keyed execution admission atomic and bounded

Objective: replace the current check-then-spawn SessionQueue behavior with one atomic ownership and admission abstraction.

Scope:

- Store one queue slot per key using atomic map entry creation.
- Give every slot an identity or generation so an old worker cannot remove a replacement.
- Bound pending work per key and across all keys.
- Use RAII reservations so cancellation or an abandoned request releases capacity.
- Provide non-blocking admission for HTTP and waiting admission for trusted internal producers.
- Keep waiting admission on dedicated producer tasks. Never call it from a
  queue work item, which could self-deadlock while waiting for its own key.
- Preserve producer FIFO ordering even though one saturated agent can delay
  other agents sharing the scheduler, DM-trigger, completion, or Telegram
  producer loop; revisit fairness only with measured operational evidence.
- Return HTTP 429 with a stable machine-readable error when capacity is exhausted.
- Reject before creating a run record, persisting a user message, or emitting run-start events.
- Preserve normal-before-low priority behavior without allowing low-priority buffers to escape capacity accounting.
- Stop accepting new work during shutdown while draining already accepted reservations and items.
- Remove idle slots only after there are no pending items, reservations, or admission waiters.

Required tests:

- More than 100 simultaneous first enqueues for one key observe exactly one active worker.
- Different keys can execute concurrently.
- Ending one generation cannot remove a newer slot for the same key.
- Per-key and global limits reject deterministically.
- Dropping an unused reservation releases both limits.
- Internal waiting admission continues after capacity is released.
- Normal and low priority ordering is preserved.
- Shutdown drains accepted work and rejects new work.
- An HTTP 429 leaves no run, message, or event side effects.

Initial limits may be conservative constants with a test-only constructor. Configuration should only be exposed after operational measurements establish useful defaults.

Phase gate:

- Targeted queue and gateway tests pass.
- Full formatting and lint checks pass.
- No unbounded channel remains inside SessionQueue.
- API documentation describes 429 behavior and retry guidance.

## Phase 2 — Establish migration and compatibility discipline

Objective: make schema evolution explicit before lifecycle state gains revisions and terminal metadata.

Scope:

- Introduce a schema-version table and ordered migrations if not already authoritative.
- Run migrations transactionally and make repeated startup idempotent.
- Add additive columns needed by later phases, including lifecycle revision and terminal reason.
- Add fixtures for the baseline schema, current schema, partially migrated failure, and restart after migration.
- Publish backup and rollback instructions.

Phase gate:

- Fresh install, baseline upgrade, repeated startup, and interrupted migration tests pass.
- The compatibility matrix names which binaries can read which schemas.

## Phase 3 — Centralize run and job lifecycle transitions

Objective: replace scattered status assignments with explicit state machines.

Scope:

- Define legal run transitions and one transition API.
- Make cancellation idempotent and prevent mark-running from resurrecting a cancelled run.
- Assign a monotonic revision to every accepted transition.
- Persist with compare-and-set or revision-aware upsert semantics.
- Ignore or reject stale snapshots instead of overwriting newer terminal state.
- Separate job execution status from terminal reason without breaking legacy readers.
- Route scheduler, HTTP, DM, and recovery paths through the same transition functions.

Required tests:

- Cancel-before-start never becomes running.
- Cancel-during-start has one terminal result.
- A stale running snapshot cannot overwrite completed, failed, or cancelled.
- Duplicate terminal transitions are harmless.
- Job completion and operator cancellation have deterministic precedence.

Phase gate:

- Direct status mutation outside the lifecycle module is eliminated or mechanically restricted.
- Persistence race tests pass under repeated execution.

## Phase 4 — Make SSE recovery authoritative

Objective: guarantee eventual browser correctness after disconnects, replay truncation, and gateway restart.

Scope:

- Add an event-stream epoch generated at gateway startup.
- Track retained event floor and newest cursor.
- Signal replay gaps and epoch mismatches explicitly.
- Reconcile from authoritative session and run snapshots while buffering live events.
- Apply buffered events after the snapshot boundary without regression.
- Give all subscriber registrations prompt drop cleanup.
- Put explicit limits on event retention and subscriber buffers.

Required tests:

- Cursor older than the retained floor forces reconciliation.
- Cursor from a prior epoch forces reconciliation.
- Events arriving during snapshot fetch are buffered and applied once.
- Repeated subscribe-and-drop during idle periods does not grow subscriber storage.
- Reconciliation removes stale activity as well as adding missing activity.

Phase gate:

- A client can recover from an arbitrary missed-event interval using documented protocol behavior.
- Browser activity indicators are derived from authoritative active run identity.

## Phase 5 — Introduce the typed frontend build without a visual rewrite

Objective: create a safe migration path from the current browser code while preserving behavior and deployment.

Scope:

- Add Vite, Preact, and strict TypeScript.
- Pin Node.js 22 and the package manager version.
- Add ESLint, Prettier, Vitest, Testing Library, and Playwright.
- Embed built assets into the Rust binary as today.
- Define Zod schemas for API payloads and SSE envelopes.
- Add a small compatibility bridge so screens can migrate incrementally.

Phase gate:

- Production build is reproducible in CI.
- Existing core user flows remain available.
- Invalid API or SSE payloads fail visibly at the boundary rather than corrupting state.

## Phase 6 — Normalize frontend state and reconnect behavior

Objective: give each server entity one client-side source of truth.

Scope:

- Normalize agents, sessions, runs, messages, jobs, and activity by ID.
- Move updates into pure reducer actions with revision or cursor guards.
- Derive lists, badges, and activity dots from normalized entities.
- Implement snapshot-plus-buffer reconciliation as a first-class state transition.
- Migrate screen by screen and remove duplicate module-level caches.

Required tests:

- Overlapping runs on one session keep activity visible until the final run ends.
- Out-of-order events cannot regress a newer entity revision.
- Agent switching does not leak prior-agent state.
- Reconnect converges to the server snapshot.
- Optimistic actions either confirm or roll back explicitly.

Phase gate:

- No screen owns an independent mutable copy of a server entity.
- Critical reconnect and multi-agent paths pass in Playwright.

## Phase 7 — Consolidate jobs, configuration, and durable operations

Objective: remove remaining split ownership of scheduler and configuration behavior.

Scope:

- Make job status and terminal reason semantics consistent across SQLite, gateway, CLI, and UI.
- Centralize configuration merge precedence and validation.
- Add transactional boundaries around multi-record durable operations.
- Add bounded retries with observable failure state for recoverable background work.
- Add metrics for queue saturation, transition rejection, replay gaps, subscribers, and persistence conflicts.

Phase gate:

- CLI, API, and UI display the same authoritative state.
- Operational saturation and recovery paths are observable.

## Phase 8 — Decompose only after invariants are enforced

Objective: improve maintainability without moving bugs between modules.

Scope:

- Split oversized gateway and runtime modules along established ownership boundaries.
- Keep lifecycle, persistence, transport, and presentation logic separate.
- Narrow public APIs and remove bypasses discovered during prior phases.
- Archive superseded design documents and update architecture diagrams.

Phase gate:

- Dependency direction remains acyclic.
- No invariant-enforcing API is bypassed by the decomposition.
- Full CI and end-to-end suites pass.

## Sequencing and delivery

Each phase should be delivered as one or more reviewable pull requests. A phase does not need to wait for unrelated documentation cleanup, but it must satisfy its own gate before the next phase relies on it.

Recommended order:

1. Phase 1 queue ownership and bounds
2. Phase 2 migration discipline
3. Phase 3 lifecycle state machines
4. Phase 4 SSE recovery
5. Phase 5 typed frontend scaffold
6. Phase 6 normalized frontend state
7. Phase 7 durable operations and observability
8. Phase 8 decomposition

Avoid running Phases 3 and 4 as independent semantic changes against the same event contracts. Frontend scaffolding in Phase 5 can begin late in Phase 4, but normalized state should consume the finalized recovery protocol.

## Effort estimate

Expected implementation effort is 18 to 30 focused agent-days:

- Phase 1: 2 to 4 agent-days
- Phase 2: 2 to 3 agent-days
- Phase 3: 3 to 5 agent-days
- Phase 4: 3 to 5 agent-days
- Phase 5: 2 to 3 agent-days
- Phase 6: 3 to 5 agent-days
- Phase 7: 2 to 3 agent-days
- Phase 8: 1 to 2 agent-days

The range assumes experienced agents, parallel review and test work, and no product redesign. Human elapsed time depends mostly on CI duration, review turnaround, and how many phases are allowed in flight. A realistic calendar range is roughly three to six weeks with disciplined review.

## Review policy

For concurrency and persistence changes, reviewers should ask for the invariant, the ownership boundary, and the deterministic regression test before discussing local implementation style.

Every pull request in this program should state:

- Which invariant it establishes.
- Which old race or ambiguity becomes impossible.
- Which capacity or lifecycle limit is introduced.
- What happens on cancellation, shutdown, and retry.
- Which compatibility promises are affected.
- How the behavior was tested.

## Non-goals

This program does not:

- Replace Rust, Tokio, Axum, SQLite, or SSE.
- Introduce a distributed control plane.
- Move to PostgreSQL without demonstrated multi-writer requirements.
- Replace Preact with React or a server-rendered framework.
- Redesign the product UI before state ownership is corrected.
- Treat the source tag as a database rollback mechanism.

## Current progress

- Open pull requests cleared before the checkpoint.
- Baseline develop commit tagged as v0.2.4-pre-stabilization.
- Phase 1 implemented on codex/fix-keyed-agent-queue with atomic keyed
  ownership, bounded admission, shutdown-safe reservations, side-effect-free
  HTTP saturation, priority-aware positions, and reentrant trigger protection.
- Phase 1 queue, gateway, coordinator, formatting, and workspace lint gates
  pass locally.
- Phase 2 is implemented on PR #1224 with transactional, versioned migrations
  and the additive lifecycle columns required by Phase 3.
- Phase 3 is implemented on codex/phase3-lifecycle-state-machines with
  authoritative run/job transition APIs, monotonic revisions, revision-aware
  SQLite upserts, stale coordinator snapshot rejection, and deterministic
  lifecycle race regressions.
- Phase 3 merged as PR #1225.
- Phase 4 merged as PR #1226 with epoch-aware replay-gap recovery,
  authoritative activity reconciliation, prompt subscriber cleanup, and
  explicit bounded-versus-lossless feed semantics.
- Post-Phase-4 develop is tagged as v0.2.4-pre-frontend-migration.
- Phase 5 is implemented on codex/phase5-typed-frontend with a reproducible
  Vite build, strict TypeScript/Zod compatibility boundary, committed
  Rust-embedded assets, and Vitest/Playwright gates.
