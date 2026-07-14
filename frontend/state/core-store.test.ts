import { describe, expect, it } from "vitest";

import type { SessionActivity } from "../contracts";
import {
  MAX_BUFFERED_ACTIVITY_EVENTS,
  createInitialCoreState,
  coreStateBridge,
  reduceCoreState,
  type CoreState,
  type JobEntity,
  type MessageEntity,
  type SessionEntity,
} from "./core-store";

const AGENT_ID = "00000000-0000-4000-8000-000000000001";
const SESSION_A = "00000000-0000-4000-8000-000000000010";
const SESSION_B = "00000000-0000-4000-8000-000000000011";
const RUN_A = "00000000-0000-4000-8000-000000000020";
const RUN_B = "00000000-0000-4000-8000-000000000021";
const EPOCH = "00000000-0000-4000-8000-000000000030";
const NEXT_EPOCH = "00000000-0000-4000-8000-000000000031";
const JOB_A = "00000000-0000-4000-8000-000000000040";
const JOB_B = "00000000-0000-4000-8000-000000000041";
const JOB_C = "00000000-0000-4000-8000-000000000042";

function session(id: string, hasActiveRun = false): SessionEntity {
  return {
    id,
    agent_id: AGENT_ID,
    context_id: `web-${id}`,
    session_type: "chat",
    has_active_run: hasActiveRun,
  };
}

function activity(sessionId: string, runId: string, hasActiveRun: boolean): SessionActivity {
  return {
    session_id: sessionId,
    run_id: runId,
    agent_id: AGENT_ID,
    has_active_run: hasActiveRun,
    ts: "2026-07-13T00:00:00Z",
  };
}

function job(id: string, status: JobEntity["status"] = "active"): JobEntity {
  return {
    id,
    prompt: "check queues",
    schedule: { type: "recurring", cron: "*/5 * * * *" },
    status,
    next_run_at: null,
    last_run_at: null,
  };
}
function reduce(state: CoreState, ...actions: Parameters<typeof reduceCoreState>[1][]): CoreState {
  return actions.reduce(reduceCoreState, state);
}

describe("normalized core reducer", () => {
  it("keeps one session entity while preserving independent ordered scopes", () => {
    const shared = session(SESSION_A);
    const state = reduceCoreState(createInitialCoreState(), {
      type: "sessions/replaced",
      agentSessions: [shared],
      crossAgentSessions: [shared, session(SESSION_B)],
    });

    expect(state.sessions.allIds).toEqual([SESSION_A, SESSION_B]);
    expect(state.sessions.agentIds).toEqual([SESSION_A]);
    expect(state.sessions.crossAgentIds).toEqual([SESSION_A, SESSION_B]);
    expect(Object.keys(state.sessions.byId)).toHaveLength(2);
  });

  it("keeps an envelope-pinned internal session visible to legacy consumers", () => {
    coreStateBridge.resetScopedState();
    const internal = { ...session(SESSION_A), session_type: "subagent" as const };

    coreStateBridge.upsertSession(internal, "pinned");

    expect(coreStateBridge.agentSessions.value).toEqual([internal]);
    coreStateBridge.resetScopedState();
  });

  it("surfaces the first live run after binding an empty new-session scope", () => {
    coreStateBridge.resetScopedState();
    coreStateBridge.replaceRuns(SESSION_A, []);
    coreStateBridge.upsertRun({
      run_id: RUN_A,
      session_id: SESSION_A,
      status: "queued",
    });

    expect(coreStateBridge.runs.value.map((run) => run.run_id)).toEqual([RUN_A]);
    expect(coreStateBridge.activeRunId.value).toBe(RUN_A);
    coreStateBridge.resetScopedState();
  });

  it("does not let an older lifecycle revision regress a terminal run", () => {
    const initial = reduceCoreState(createInitialCoreState(), {
      type: "runs/replaced",
      sessionId: SESSION_A,
      runs: [
        {
          run_id: RUN_A,
          session_id: SESSION_A,
          status: "completed",
          lifecycle_revision: 5,
        },
      ],
    });
    const state = reduceCoreState(initial, {
      type: "run/upserted",
      run: {
        run_id: RUN_A,
        session_id: SESSION_A,
        status: "running",
        lifecycle_revision: 4,
      },
      cursor: null,
      streamEpoch: null,
    });

    expect(state.runs.byId[RUN_A]?.status).toBe("completed");
    expect(state.runs.byId[RUN_A]?.lifecycle_revision).toBe(5);
  });

  it("ignores repeated or out-of-order run events within one epoch", () => {
    const running = reduceCoreState(createInitialCoreState(), {
      type: "run/upserted",
      run: { run_id: RUN_A, session_id: SESSION_A, status: "running" },
      cursor: 10,
      streamEpoch: EPOCH,
    });
    const stale = reduceCoreState(running, {
      type: "run/upserted",
      run: { run_id: RUN_A, session_id: SESSION_A, status: "completed" },
      cursor: 9,
      streamEpoch: EPOCH,
    });

    expect(stale).toBe(running);
    expect(stale.runs.byId[RUN_A]?.status).toBe("running");
  });

  it("keeps overlapping-session activity until the authoritative final end", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "activity/snapshot",
      sessions: [session(SESSION_A)],
      cursor: 0,
      streamEpoch: EPOCH,
    });
    state = reduce(
      state,
      {
        type: "activity/event",
        event: {
          type: "session_activity_started",
          data: activity(SESSION_A, RUN_A, true),
          cursor: 1,
          streamEpoch: EPOCH,
        },
      },
      {
        type: "activity/event",
        event: {
          type: "session_activity_started",
          data: activity(SESSION_A, RUN_B, true),
          cursor: 2,
          streamEpoch: EPOCH,
        },
      },
      {
        type: "activity/event",
        event: {
          type: "session_activity_ended",
          data: activity(SESSION_A, RUN_A, true),
          cursor: 3,
          streamEpoch: EPOCH,
        },
      },
    );

    expect(state.activity.bySessionId[SESSION_A]).toMatchObject({
      hasActiveRun: true,
      runId: null,
    });

    state = reduceCoreState(state, {
      type: "activity/event",
      event: {
        type: "session_activity_ended",
        data: activity(SESSION_A, RUN_B, false),
        cursor: 4,
        streamEpoch: EPOCH,
      },
    });
    expect(state.activity.bySessionId[SESSION_A]?.hasActiveRun).toBe(false);
  });

  it("commits a snapshot before applying only buffered events above its ceiling", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "activity/snapshot",
      sessions: [session(SESSION_A, true)],
      cursor: 5,
      streamEpoch: EPOCH,
    });
    state = reduce(
      state,
      {
        type: "activity/reconciliation-began",
        token: 7,
        replayCeiling: 10,
        streamEpoch: EPOCH,
      },
      {
        type: "activity/event",
        event: {
          type: "session_activity_started",
          data: activity(SESSION_A, RUN_A, true),
          cursor: 9,
          streamEpoch: EPOCH,
        },
      },
      {
        type: "activity/event",
        event: {
          type: "session_activity_started",
          data: activity(SESSION_B, RUN_B, true),
          cursor: 11,
          streamEpoch: EPOCH,
        },
      },
      {
        type: "activity/reconciliation-committed",
        token: 7,
        sessions: [session(SESSION_A), session(SESSION_B)],
      },
    );

    expect(state.activity.bySessionId[SESSION_A]?.hasActiveRun).toBe(false);
    expect(state.activity.bySessionId[SESSION_B]?.hasActiveRun).toBe(true);
    expect(state.activity.appliedCursor).toBe(11);
    expect(state.activityReconciliation).toBeNull();
  });

  it("authoritative snapshots remove stale activity and scoped reset isolates agents", () => {
    const active = reduceCoreState(createInitialCoreState(), {
      type: "activity/snapshot",
      sessions: [session(SESSION_A, true)],
      cursor: 5,
      streamEpoch: EPOCH,
    });
    const reconciled = reduceCoreState(active, {
      type: "activity/snapshot",
      sessions: [session(SESSION_B)],
      cursor: 8,
      streamEpoch: EPOCH,
    });
    expect(reconciled.activity.bySessionId[SESSION_A]).toBeUndefined();

    const reset = reduceCoreState(reconciled, { type: "scoped/reset" });
    expect(reset.sessions.allIds).toEqual([]);
    expect(reset.runs.allIds).toEqual([]);
    expect(reset.activity.bySessionId).toEqual({});
  });

  it("bounds reconciliation buffering and records overflow for a forced retry", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "activity/reconciliation-began",
      token: 9,
      replayCeiling: 10,
      streamEpoch: EPOCH,
    });
    const event = {
      type: "activity/event" as const,
      event: {
        type: "session_activity_started" as const,
        data: activity(SESSION_A, RUN_A, true),
        cursor: 11,
        streamEpoch: EPOCH,
      },
    };
    for (let index = 0; index <= MAX_BUFFERED_ACTIVITY_EVENTS; index++) {
      state = reduceCoreState(state, event);
    }

    expect(state.activityReconciliation?.buffered).toHaveLength(MAX_BUFFERED_ACTIVITY_EVENTS);
    expect(state.activityReconciliation?.overflowed).toBe(true);
  });
  it("resets the activity cursor watermark when the stream epoch changes", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "activity/snapshot",
      sessions: [session(SESSION_A)],
      cursor: 100,
      streamEpoch: EPOCH,
    });
    state = reduce(
      state,
      {
        type: "activity/event",
        event: {
          type: "session_activity_started",
          data: activity(SESSION_A, RUN_A, true),
          cursor: 1,
          streamEpoch: NEXT_EPOCH,
        },
      },
      {
        type: "activity/event",
        event: {
          type: "session_activity_ended",
          data: activity(SESSION_A, RUN_A, false),
          cursor: 2,
          streamEpoch: NEXT_EPOCH,
        },
      },
    );

    expect(state.activity.appliedCursor).toBe(2);
    expect(state.activity.streamEpoch).toBe(NEXT_EPOCH);
    expect(state.activity.bySessionId[SESSION_A]?.hasActiveRun).toBe(false);
  });

  it("does not compare a buffered new-epoch cursor with the old replay ceiling", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "activity/reconciliation-began",
      token: 11,
      replayCeiling: 100,
      streamEpoch: EPOCH,
    });
    state = reduce(
      state,
      {
        type: "activity/event",
        event: {
          type: "session_activity_started",
          data: activity(SESSION_A, RUN_A, true),
          cursor: 1,
          streamEpoch: NEXT_EPOCH,
        },
      },
      { type: "activity/reconciliation-committed", token: 11, sessions: [session(SESSION_A)] },
    );

    expect(state.activity.bySessionId[SESSION_A]?.hasActiveRun).toBe(true);
    expect(state.activity.appliedCursor).toBe(1);
    expect(state.activity.streamEpoch).toBe(NEXT_EPOCH);
  });

  it("keeps reconciliation abort fail-closed by discarding buffered activity", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "activity/snapshot",
      sessions: [session(SESSION_A)],
      cursor: 5,
      streamEpoch: EPOCH,
    });
    state = reduce(
      state,
      { type: "activity/reconciliation-began", token: 12, replayCeiling: 5, streamEpoch: EPOCH },
      {
        type: "activity/event",
        event: {
          type: "session_activity_started",
          data: activity(SESSION_A, RUN_A, true),
          cursor: 6,
          streamEpoch: EPOCH,
        },
      },
      { type: "activity/reconciliation-aborted", token: 12 },
    );

    expect(state.activity.bySessionId[SESSION_A]?.hasActiveRun).toBe(false);
    expect(state.activity.appliedCursor).toBe(5);
    expect(state.activityReconciliation).toBeNull();
  });

  it("ignores a reconciliation commit with a mismatched token", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "activity/reconciliation-began",
      token: 13,
      replayCeiling: 5,
      streamEpoch: EPOCH,
    });
    state = reduceCoreState(state, {
      type: "activity/event",
      event: {
        type: "session_activity_started",
        data: activity(SESSION_A, RUN_A, true),
        cursor: 6,
        streamEpoch: EPOCH,
      },
    });
    const unchanged = reduceCoreState(state, {
      type: "activity/reconciliation-committed",
      token: 99,
      sessions: [session(SESSION_A)],
    });

    expect(unchanged).toBe(state);
    expect(unchanged.activityReconciliation?.token).toBe(13);
    expect(unchanged.activityReconciliation?.buffered).toHaveLength(1);
  });

  it("normalizes ordered messages per session without duplicate entities", () => {
    const first: MessageEntity = { id: "message-1", type: "user", text: "first" };
    const second: MessageEntity = { id: "message-2", type: "agent", text: "second" };
    let state = reduceCoreState(createInitialCoreState(), {
      type: "messages/replaced",
      sessionId: SESSION_A,
      messages: [first, second, { ...first, text: "updated" }],
    });

    expect(state.messages.idsBySession[SESSION_A]).toEqual(["message-1", "message-2"]);
    expect(state.messages.byId["message-1"]?.text).toBe("updated");
    expect(state.messages.allIds).toHaveLength(2);

    state = reduceCoreState(state, {
      type: "messages/replaced",
      sessionId: SESSION_B,
      messages: [{ id: "message-3", type: "system", text: "other session" }],
    });
    expect(state.messages.visibleSessionId).toBe(SESSION_B);
    expect(state.messages.byId["message-1"]?.text).toBe("updated");
    expect(state.messages.byId["message-3"]?.text).toBe("other session");
  });

  it("preserves an optimistic message through snapshots until explicit confirmation", () => {
    const optimistic: MessageEntity = {
      id: "message-optimistic",
      type: "user",
      text: "send this",
      optimistic: true,
    };
    let state = reduceCoreState(createInitialCoreState(), {
      type: "message/optimistic-began",
      sessionId: SESSION_A,
      message: optimistic,
      pending: {
        messageId: optimistic.id,
        text: "send this",
        runId: null,
        ts: "2026-07-13T00:00:00Z",
      },
      companions: [{ id: "thinking-1", type: "thinking", pending: true }],
    });
    state = reduceCoreState(state, {
      type: "messages/replaced",
      sessionId: SESSION_A,
      messages: [],
    });

    expect(state.messages.idsBySession[SESSION_A]).toEqual(["message-optimistic"]);
    state = reduce(
      state,
      {
        type: "message/optimistic-linked",
        sessionId: SESSION_A,
        messageId: optimistic.id,
        runId: RUN_A,
      },
      {
        type: "message/optimistic-confirmed",
        sessionId: SESSION_A,
        correlation: { runId: RUN_A },
      },
    );
    expect(state.messages.pendingBySession[SESSION_A]).toBeUndefined();
    expect(state.messages.byId["message-optimistic"]?.optimistic).toBe(false);

    state = reduceCoreState(state, {
      type: "messages/replaced",
      sessionId: SESSION_A,
      messages: [],
    });
    expect(state.messages.idsBySession[SESSION_A]).toEqual([]);
  });

  it("settles a cancelled optimistic message when terminal SSE beats run correlation", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "message/optimistic-began",
      sessionId: SESSION_A,
      message: {
        id: "message-racing",
        type: "user",
        text: "cancel after accept",
        optimistic: true,
      },
      pending: {
        messageId: "message-racing",
        text: "cancel after accept",
        runId: null,
        ts: "2026-07-13T00:00:00Z",
      },
      companions: [],
    });
    state = reduce(
      state,
      {
        type: "run/upserted",
        run: {
          run_id: RUN_A,
          session_id: SESSION_A,
          status: "cancelled",
        },
        cursor: 12,
        streamEpoch: EPOCH,
      },
      {
        type: "message/optimistic-linked",
        sessionId: SESSION_A,
        messageId: "message-racing",
        runId: RUN_A,
      },
    );

    expect(state.messages.pendingBySession[SESSION_A]).toBeUndefined();
    expect(state.messages.byId["message-racing"]).toMatchObject({
      optimistic: false,
    });
    expect(state.messages.byId["message-racing"]?.failed).toBeUndefined();
  });

  it("removes an optimistic duplicate using the persisted run correlation", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "message/optimistic-began",
      sessionId: SESSION_A,
      message: {
        id: "message-optimistic-duplicate",
        type: "user",
        text: "same input",
        optimistic: true,
      },
      pending: {
        messageId: "message-optimistic-duplicate",
        text: "same input",
        runId: null,
        ts: "2026-07-13T00:00:00Z",
      },
      companions: [],
    });
    state = reduce(
      state,
      {
        type: "messages/replaced",
        sessionId: SESSION_A,
        messages: [
          {
            id: "message-persisted",
            type: "user",
            text: "same input",
            metadata: { run_id: RUN_A },
          },
        ],
      },
      {
        type: "run/upserted",
        run: { run_id: RUN_A, session_id: SESSION_A, status: "completed" },
        cursor: 13,
        streamEpoch: EPOCH,
      },
      {
        type: "message/optimistic-linked",
        sessionId: SESSION_A,
        messageId: "message-optimistic-duplicate",
        runId: RUN_A,
      },
    );

    expect(state.messages.idsBySession[SESSION_A]).toEqual(["message-persisted"]);
    expect(state.messages.byId["message-optimistic-duplicate"]).toBeUndefined();
    expect(state.messages.allIds).not.toContain("message-optimistic-duplicate");
    expect(state.messages.pendingBySession[SESSION_A]).toBeUndefined();
  });

  it("marks a failed optimistic message through an explicit rollback", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "message/optimistic-began",
      sessionId: SESSION_A,
      message: {
        id: "message-failed",
        type: "user",
        text: "will fail",
        optimistic: true,
      },
      pending: {
        messageId: "message-failed",
        text: "will fail",
        runId: null,
        ts: "2026-07-13T00:00:00Z",
      },
      companions: [],
    });
    state = reduceCoreState(state, {
      type: "message/optimistic-rolled-back",
      sessionId: SESSION_A,
      correlation: { messageId: "message-failed" },
    });

    expect(state.messages.pendingBySession[SESSION_A]).toBeUndefined();
    expect(state.messages.byId["message-failed"]).toMatchObject({
      optimistic: false,
      failed: true,
    });
  });

  it("normalizes jobs and settles optimistic creates and cancellations", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "jobs/replaced",
      jobs: [job(JOB_A), job(JOB_B), { ...job(JOB_A), prompt: "updated" }],
      mutationGeneration: 0,
    });
    expect(state.jobs.allIds).toEqual([JOB_A, JOB_B]);
    expect(state.jobs.byId[JOB_A]?.prompt).toBe("updated");

    state = reduceCoreState(state, {
      type: "job/optimistic-created",
      job: { ...job("optimistic-job-1", "pending"), optimistic: true },
    });
    expect(state.jobs.allIds[0]).toBe("optimistic-job-1");
    state = reduceCoreState(state, {
      type: "job/optimistic-create-confirmed",
      optimisticId: "optimistic-job-1",
      job: job(JOB_C),
    });
    expect(state.jobs.byId["optimistic-job-1"]).toBeUndefined();
    expect(state.jobs.byId[JOB_C]?.status).toBe("active");

    state = reduceCoreState(state, { type: "job/optimistic-cancelled", jobId: JOB_A });
    expect(state.jobs.byId[JOB_A]?.status).toBe("cancelled");
    state = reduceCoreState(state, {
      type: "job/optimistic-cancel-rolled-back",
      jobId: JOB_A,
    });
    expect(state.jobs.byId[JOB_A]?.status).toBe("active");

    state = reduce(
      state,
      { type: "job/optimistic-cancelled", jobId: JOB_A },
      {
        type: "job/optimistic-cancel-confirmed",
        jobId: JOB_A,
        job: { ...job(JOB_A, "cancelled"), lifecycle_revision: 2 },
      },
    );
    expect(state.jobs.byId[JOB_A]?.status).toBe("cancelled");
    expect(state.jobs.byId[JOB_A]?.optimistic).toBeUndefined();
  });

  it("settles overlapping optimistic messages by exact run identity", () => {
    let state = createInitialCoreState();
    for (const [messageId, text] of [
      ["message-first", "first"],
      ["message-second", "second"],
    ] as const) {
      state = reduceCoreState(state, {
        type: "message/optimistic-began",
        sessionId: SESSION_A,
        message: { id: messageId, type: "user", text, optimistic: true },
        pending: {
          messageId,
          text,
          runId: null,
          ts: "2026-07-13T00:00:00Z",
        },
        companions: [],
      });
    }
    state = reduce(
      state,
      {
        type: "message/optimistic-linked",
        sessionId: SESSION_A,
        messageId: "message-first",
        runId: RUN_A,
      },
      {
        type: "message/optimistic-linked",
        sessionId: SESSION_A,
        messageId: "message-second",
        runId: RUN_B,
      },
      {
        type: "message/optimistic-confirmed",
        sessionId: SESSION_A,
        correlation: { runId: RUN_A },
      },
    );

    expect(state.messages.pendingBySession[SESSION_A]).toEqual([
      expect.objectContaining({ messageId: "message-second", runId: RUN_B }),
    ]);
    expect(state.messages.byId["message-first"]?.optimistic).toBe(false);
    expect(state.messages.byId["message-second"]?.optimistic).toBe(true);

    state = reduceCoreState(state, {
      type: "message/optimistic-confirmed",
      sessionId: SESSION_A,
      correlation: { runId: RUN_B },
    });
    expect(state.messages.pendingBySession[SESSION_A]).toBeUndefined();
    expect(state.messages.byId["message-second"]?.optimistic).toBe(false);
  });

  it("rejects stale job snapshots and rolls cancellation back to the newest accepted snapshot", () => {
    let state = reduceCoreState(createInitialCoreState(), {
      type: "jobs/replaced",
      jobs: [job(JOB_A)],
      mutationGeneration: 0,
    });
    const staleGeneration = state.jobs.mutationGeneration;
    state = reduceCoreState(state, { type: "job/optimistic-cancelled", jobId: JOB_A });

    state = reduceCoreState(state, {
      type: "jobs/replaced",
      jobs: [{ ...job(JOB_A), prompt: "new snapshot", lifecycle_revision: 7 }],
      mutationGeneration: state.jobs.mutationGeneration,
    });
    state = reduceCoreState(state, {
      type: "job/optimistic-cancel-rolled-back",
      jobId: JOB_A,
    });
    expect(state.jobs.byId[JOB_A]).toMatchObject({
      prompt: "new snapshot",
      lifecycle_revision: 7,
      status: "active",
    });

    const afterRollback = state;
    state = reduceCoreState(state, {
      type: "jobs/replaced",
      jobs: [],
      mutationGeneration: staleGeneration,
    });
    expect(state).toBe(afterRollback);
    expect(state.jobs.byId[JOB_A]).toBeDefined();
  });

  it("clears cached messages and pending work for an inactive session", () => {
    const state = reduce(
      createInitialCoreState(),
      {
        type: "messages/replaced",
        sessionId: SESSION_A,
        messages: [{ id: "inactive-history", type: "user", text: "old" }],
      },
      {
        type: "message/optimistic-began",
        sessionId: SESSION_A,
        message: {
          id: "inactive-pending",
          type: "user",
          text: "pending",
          optimistic: true,
        },
        pending: {
          messageId: "inactive-pending",
          text: "pending",
          runId: null,
          ts: "2026-07-13T00:00:00Z",
        },
        companions: [],
      },
      {
        type: "messages/replaced",
        sessionId: SESSION_B,
        messages: [{ id: "visible-history", type: "user", text: "keep" }],
      },
      {
        type: "runs/replaced",
        sessionId: SESSION_A,
        runs: [{ run_id: RUN_A, session_id: SESSION_A, status: "completed" }],
      },
      {
        type: "runs/replaced",
        sessionId: SESSION_B,
        runs: [{ run_id: RUN_B, session_id: SESSION_B, status: "running" }],
      },
      { type: "runs/cleared", sessionId: SESSION_A },
      { type: "messages/cleared", sessionId: SESSION_A },
    );

    expect(state.messages.visibleSessionId).toBe(SESSION_B);
    expect(state.messages.idsBySession[SESSION_A]).toBeUndefined();
    expect(state.messages.pendingBySession[SESSION_A]).toBeUndefined();
    expect(state.messages.byId["inactive-history"]).toBeUndefined();
    expect(state.messages.byId["inactive-pending"]).toBeUndefined();
    expect(state.messages.byId["visible-history"]?.text).toBe("keep");
    expect(state.runs.idsBySession[SESSION_A]).toBeUndefined();
    expect(state.runs.byId[RUN_A]).toBeUndefined();
    expect(state.runs.byId[RUN_B]?.status).toBe("running");
  });
  it("does not notify unrelated job selectors for message-only updates", () => {
    coreStateBridge.replaceJobs([], coreStateBridge.getJobMutationGeneration());
    let notifications = 0;
    const unsubscribe = coreStateBridge.jobs.subscribe(() => {
      notifications += 1;
    });

    coreStateBridge.replaceMessages(SESSION_A, [
      { id: "selector-message", type: "agent", text: "one" },
    ]);
    coreStateBridge.transformMessages(SESSION_A, (messages) =>
      messages.map((message) => ({ ...message, text: "two" })),
    );

    expect(notifications).toBe(1);
    unsubscribe();
    coreStateBridge.clearMessages(SESSION_A);
  });
});
