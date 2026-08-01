# Phase 7 — Durable operations and observability

Status: implementation and validation complete on `codex/phase7-durable-operations`.

## Objective

Remove the remaining split ownership between durable state, scheduler state,
runtime configuration, and operator-facing diagnostics. SQLite remains the
authoritative durable store; queues, scheduler registrations, and frontend
state are projections that must converge after cancellation or restart.

The project is not deployed and has no compatibility requirement. Phase 7
therefore removes the misleading legacy job representation directly instead
of preserving `cancelled` as an alias for successful completion.

## Invariants

1. A completed job is `completed`; an operator-cancelled job is `cancelled`;
   an exhausted job is `failed`. `terminal_reason` refines a terminal status
   and never contradicts it.
2. A queued interactive run and its visible input message are committed in
   one SQLite transaction before either appears in memory or enters a queue.
3. Scheduler registrations are recoverable projections of a persisted job
   and `next_run_at`. Restarting the gateway reconstructs the projection.
4. Recoverable job-dispatch failures use a bounded retry budget. Retry count,
   last error, next retry, and exhaustion are part of authoritative job state.
5. Provider/model precedence and summary-pair validation are implemented by
   shared policy functions used by create, update, settings, and run paths.
6. Queue saturation, transition rejection, replay gaps, subscriber counts,
   persistence conflicts, retries, and exhausted recovery are queryable.

## Delivery

### Slice A — jobs and durable run admission

- Add explicit `completed` and `failed` job states.
- Migrate legacy completed/deadline rows to `completed`.
- Add persisted retry metadata and bounded exponential retry for job dispatch.
- Persist recurring jobs with their first `next_run_at` in the initial write.
- Commit queued runs, input messages, and session activity timestamps in one
  SQLite transaction.
- Update the API contract, CLI, UI, tests, and documentation.

### Slice B — configuration authority and operational metrics

- Extract shared provider/model and summary-pair policy from HTTP handlers.
- Route config validation through those policies before committing live state.
- Expose a protected operational metrics snapshot.
- Instrument queue admission, lifecycle rejection, replay gaps, subscriber
  gauges, persistence conflict rejection, retry attempts, and exhaustion.

The slices may land in one PR when the combined diff remains reviewable. If
review size becomes the limiting factor, Slice B will follow as a dependent
PR; Phase 8 must not start until both phase gates pass.

## Failure, cancellation, and shutdown

- Operator cancellation is terminal and wins races with completion/retry.
- Successful completion and retry exhaustion are terminal and reject later
  scheduler writes.
- A retry is persisted before its scheduler projection is installed. If
  shutdown interrupts projection, startup reconciliation schedules it again.
- Retry delay is bounded exponential backoff with a finite attempt count.
- Transaction failure leaves neither a queued run nor a pending input message
  committed or visible.

## Required tests

- Completed, cancelled, failed, and deadline-completed jobs round-trip through
  SQLite and render consistently through API contracts and CLI formatting.
- Legacy completed-as-cancelled rows migrate to `completed` transactionally.
- A failed job dispatch retries only up to the configured bound and then
  becomes observably failed.
- A persistence failure in queued run admission rolls back both the run and
  input message.
- A restart reconstructs pending retry/scheduler state from SQLite.
- Shared config policy returns identical decisions for agent and settings
  callers.
- Metrics increment deterministically for each instrumented failure path and
  subscriber gauges return to zero after drop.

## Phase gate

- SQLite, API, CLI, and UI report the same job lifecycle and failure detail.
- No interactive run can be durable without its accepted input, or vice versa.
- Background recovery terminates and exposes its final state.
- Operational saturation and convergence paths are visible through one
  authenticated endpoint.
