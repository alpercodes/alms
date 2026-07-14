# Phase 6 — Normalized frontend state and reconnect behavior

Status: implementation complete; Phase 6B pending review and merge

Parent plan: `docs/engineering-stabilization-plan.md`

## Objective

Give each server entity one client-side representation and make reconnect a
tested state transition rather than a collection of screen-specific resets.
The visual UI and backend wire protocol remain unchanged.

## Delivery split

Phase 6 is intentionally delivered as two reviewable pull requests.

### Phase 6A — Core entities and recovery

- Add a strict TypeScript normalized store for agents, sessions, runs, and
  session activity.
- Expose read-only compatibility signals to the existing JavaScript screens.
- Replace direct array/map assignments with typed store actions.
- Guard run updates by lifecycle revision or stream cursor.
- Move activity snapshot-plus-buffer reconciliation into the reducer.
- Handle session-stream epoch/replay-gap signals through authoritative reload.
- Derive active-run selection and sidebar activity from normalized entities.

### Phase 6B — Messages, jobs, and screen completion

- Normalize per-session messages and jobs by ID.
- Move optimistic message/job operations to explicit confirm/rollback actions.
- Migrate remaining screen-local entity copies to selectors.
- Remove superseded module-level caches and compatibility writers.
- Add browser tests for reconnect convergence and agent-switch isolation.

Phase 6 is complete only when both PRs satisfy the parent plan's phase gate.

## State model

The store owns normalized tables and ordered ID indexes:

- `agents.byId` and `agents.allIds`
- `sessions.byId`, active-agent session IDs, and cross-agent session IDs
- `runs.byId` and ordered run IDs per session
- `activity.bySessionId`
- Phase 6B: `messages.byId`, ordered message IDs per session, and `jobs.byId`

UI-only selection remains separate state: active agent/session, expanded groups,
selected run, panel state, drafts, and theme are not server entities.

Compatibility signals are computed selectors over the normalized store. They
are read-only. Legacy screens may read them during migration but all writes go
through named store actions.

## Reducer invariants

1. A server entity has exactly one object in one normalized table.
2. Ordered lists contain IDs only and never independent entity copies.
3. A run payload with a lower `lifecycle_revision` cannot replace a newer run.
4. Within one stream epoch, an event cursor at or below the applied cursor is
   ignored.
5. An authoritative snapshot may remove stale activity as well as add it.
6. While reconciliation is active, live activity events are buffered.
7. Committing reconciliation applies the snapshot first, then only buffered
   events newer than the snapshot/replay ceiling.
8. Switching agents atomically clears scoped sessions, runs, activity, and
   message selection before the new snapshot is installed.
9. Store actions are pure and deterministic; signal publication occurs once
   per logical action.
10. Optimistic messages are correlated by message ID until a run ID exists,
    then settled only by that exact run ID.
11. A job snapshot may commit only if no job mutation began or settled since
    the request captured its mutation generation.
12. Entity selectors subscribe to their own state slice, so high-frequency
    message updates cannot notify unrelated agent/session/run/job consumers.

## Reconnect transition

```text
connected
  -> stream_state(replay gap or epoch mismatch)
  -> begin reconciliation(token, replay ceiling)
  -> buffer validated live events
  -> fetch authoritative REST snapshot
  -> commit snapshot
  -> apply buffered events with cursor > ceiling
  -> connected
```

If the snapshot fails, the store keeps the prior state, retains or aborts the
reconciliation token explicitly, and the stream-health path retries with
bounded backoff. It never advances past a rejected frame using a partial
snapshot.

For the per-session stream, authoritative reload includes session metadata,
runs, messages, tool calls, approvals, and in-flight text/reasoning. The stream
reopens only after that strict reload succeeds.

## Migration order

1. Export inferred wire types from the Phase 5 Zod contracts.
2. Implement and unit-test the pure normalized reducer.
3. Install a mandatory typed state bridge before the legacy app module.
4. Convert agent/session/run/activity state modules into read-only selectors.
5. Migrate boot, agent switching, session navigation, and REST snapshots.
6. Migrate global activity events and reconciliation.
7. Migrate per-session run lifecycle events and replay-gap recovery.
8. Rebuild embedded assets and run all frontend/Rust gates.
9. In Phase 6B, migrate messages/jobs and finish browser-level tests.

## Phase 6A tests

- Normalization preserves ordering while deduplicating entity IDs.
- A lower lifecycle revision cannot regress a run.
- A repeated or older stream cursor is ignored.
- Two active runs on one session keep activity visible until the authoritative
  final-run event clears it.
- Snapshot reconciliation removes stale activity.
- Events arriving during snapshot fetch apply once after the snapshot.
- Events at or below the replay ceiling do not regress the snapshot.
- Agent reset removes prior-agent scoped entities and activity.
- Epoch/replay-gap session recovery performs a strict authoritative reload.
- Existing UI behavior, contract, build, and embedding tests remain green.

## Phase 6B tests

- Message normalization preserves per-session ordering and deduplicates IDs.
- Authoritative history snapshots retain an unconfirmed optimistic message.
- Optimistic messages and jobs confirm or roll back explicitly.
- Job snapshots preserve in-flight optimistic create/cancel transitions,
  reject pre-mutation responses, and update rollback baselines safely.
- Job mutation responses contain the final persisted entity, including
  lifecycle revision, next run, and terminal reason.
- Overlapping optimistic messages settle by exact run identity, and the UI
  blocks a second uncorrelated submission until the first has a run ID.
- Message-only updates do not notify unrelated entity selectors.
- Deleting any session clears its cached messages and pending operations.
- Agent switching removes prior-agent message entities before rendering the
  next session.
- A replay-gap reconnect replaces stale messages with the authoritative
  session snapshot.
- Existing UI behavior, typed contracts, browser flows, and embedded assets
  remain green.

## Phase gate

Phase 6 is complete when:

- no screen owns an independent mutable copy of a server entity;
- all entity writes use typed reducer actions;
- reconnect and agent switching converge deterministically;
- critical reconnect and multi-agent Playwright scenarios pass;
- checked-in embedded assets reproduce exactly in CI.
