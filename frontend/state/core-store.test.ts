import { describe, expect, it } from "vitest";

import type { SessionActivity } from "../contracts";
import {
  MAX_BUFFERED_ACTIVITY_EVENTS,
  createInitialCoreState,
  coreStateBridge,
  reduceCoreState,
  type CoreState,
  type SessionEntity,
} from "./core-store";

const AGENT_ID = "00000000-0000-4000-8000-000000000001";
const SESSION_A = "00000000-0000-4000-8000-000000000010";
const SESSION_B = "00000000-0000-4000-8000-000000000011";
const RUN_A = "00000000-0000-4000-8000-000000000020";
const RUN_B = "00000000-0000-4000-8000-000000000021";
const EPOCH = "00000000-0000-4000-8000-000000000030";
const NEXT_EPOCH = "00000000-0000-4000-8000-000000000031";

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
});
