import { computed, signal, type ReadonlySignal } from "@preact/signals";

import type { Agent, Session, SessionActivity } from "../contracts";

export type RunStatus = "queued" | "running" | "completed" | "failed" | "cancelled";
export type AgentEntity = Pick<Agent, "id" | "name" | "is_default"> &
  Partial<Agent> &
  Record<string, unknown>;
export type SessionEntity = Session;
export type RunEntity = {
  run_id: string;
  session_id?: string;
  status: RunStatus;
  lifecycle_revision?: number;
  [key: string]: unknown;
};

export interface EntityTable<T> {
  readonly byId: Readonly<Record<string, T>>;
  readonly allIds: readonly string[];
}

export interface ActivityRecord {
  readonly sessionId: string;
  readonly runId: string | null;
  readonly hasActiveRun: boolean;
  readonly cursor: number | null;
  readonly streamEpoch: string | null;
}

export interface ActivityTransition {
  readonly type: "session_activity_started" | "session_activity_ended";
  readonly data: SessionActivity;
  readonly cursor: number | null;
  readonly streamEpoch: string | null;
}

interface ActivityReconciliation {
  readonly token: number;
  readonly replayCeiling: number | null;
  readonly streamEpoch: string | null;
  readonly buffered: readonly ActivityTransition[];
  readonly overflowed: boolean;
}

export interface CoreState {
  readonly agents: EntityTable<AgentEntity>;
  readonly sessions: EntityTable<SessionEntity> & {
    readonly agentIds: readonly string[];
    readonly crossAgentIds: readonly string[];
    readonly pinnedIds: readonly string[];
  };
  readonly runs: EntityTable<RunEntity> & {
    readonly idsBySession: Readonly<Record<string, readonly string[]>>;
    readonly visibleSessionId: string | null;
    readonly cursorById: Readonly<
      Record<string, { readonly cursor: number; readonly streamEpoch: string | null }>
    >;
  };
  readonly activity: {
    readonly bySessionId: Readonly<Record<string, ActivityRecord>>;
    readonly appliedCursor: number | null;
    readonly streamEpoch: string | null;
  };
  readonly activityReconciliation: ActivityReconciliation | null;
}

export type CoreAction =
  | { readonly type: "agents/replaced"; readonly agents: readonly AgentEntity[] }
  | {
      readonly type: "sessions/replaced";
      readonly agentSessions: readonly SessionEntity[];
      readonly crossAgentSessions: readonly SessionEntity[];
    }
  | {
      readonly type: "session/upserted";
      readonly session: SessionEntity;
      readonly scope: "agent" | "cross" | "pinned";
    }
  | { readonly type: "scoped/reset" }
  | {
      readonly type: "runs/replaced";
      readonly sessionId: string;
      readonly runs: readonly RunEntity[];
    }
  | { readonly type: "runs/cleared"; readonly sessionId?: string }
  | {
      readonly type: "run/upserted";
      readonly run: RunEntity;
      readonly cursor: number | null;
      readonly streamEpoch: string | null;
    }
  | {
      readonly type: "activity/snapshot";
      readonly sessions: readonly SessionEntity[];
      readonly cursor: number | null;
      readonly streamEpoch: string | null;
    }
  | { readonly type: "activity/event"; readonly event: ActivityTransition }
  | {
      readonly type: "activity/reconciliation-began";
      readonly token: number;
      readonly replayCeiling: number | null;
      readonly streamEpoch: string | null;
    }
  | {
      readonly type: "activity/reconciliation-committed";
      readonly token: number;
      readonly sessions: readonly SessionEntity[];
    }
  | { readonly type: "activity/reconciliation-aborted"; readonly token: number };

export const MAX_BUFFERED_ACTIVITY_EVENTS = 2_048;

function emptyTable<T>(): EntityTable<T> {
  return { byId: {}, allIds: [] };
}

export function createInitialCoreState(): CoreState {
  return {
    agents: emptyTable<AgentEntity>(),
    sessions: {
      ...emptyTable<SessionEntity>(),
      agentIds: [],
      crossAgentIds: [],
      pinnedIds: [],
    },
    runs: {
      ...emptyTable<RunEntity>(),
      idsBySession: {},
      visibleSessionId: null,
      cursorById: {},
    },
    activity: {
      bySessionId: {},
      appliedCursor: null,
      streamEpoch: null,
    },
    activityReconciliation: null,
  };
}

function uniqueIds<T>(items: readonly T[], idOf: (item: T) => string): string[] {
  return [...new Set(items.map(idOf))];
}
function replaceAgents(state: CoreState, agents: readonly AgentEntity[]): CoreState {
  const byId: Record<string, AgentEntity> = {};
  for (const agent of agents) byId[agent.id] = agent;
  return {
    ...state,
    agents: { byId, allIds: uniqueIds(agents, (agent) => agent.id) },
  };
}

function replaceSessions(
  state: CoreState,
  agentSessions: readonly SessionEntity[],
  crossAgentSessions: readonly SessionEntity[],
): CoreState {
  const byId: Record<string, SessionEntity> = {};
  for (const id of state.sessions.pinnedIds) {
    const session = state.sessions.byId[id];
    if (session) byId[id] = session;
  }
  for (const session of agentSessions) byId[session.id] = session;
  for (const session of crossAgentSessions) byId[session.id] = session;
  return {
    ...state,
    sessions: {
      byId,
      allIds: Object.keys(byId),
      agentIds: uniqueIds(agentSessions, (session) => session.id),
      crossAgentIds: uniqueIds(crossAgentSessions, (session) => session.id),
      pinnedIds: state.sessions.pinnedIds.filter((id) => Boolean(byId[id])),
    },
  };
}

function appendUnique(ids: readonly string[], id: string): readonly string[] {
  return ids.includes(id) ? ids : [...ids, id];
}

function upsertSession(
  state: CoreState,
  session: SessionEntity,
  scope: "agent" | "cross" | "pinned",
): CoreState {
  const agentIds =
    scope === "agent" ? appendUnique(state.sessions.agentIds, session.id) : state.sessions.agentIds;
  const crossAgentIds =
    scope === "cross"
      ? appendUnique(state.sessions.crossAgentIds, session.id)
      : state.sessions.crossAgentIds;
  const pinnedIds =
    scope === "pinned"
      ? appendUnique(state.sessions.pinnedIds, session.id)
      : state.sessions.pinnedIds;
  return {
    ...state,
    sessions: {
      ...state.sessions,
      byId: { ...state.sessions.byId, [session.id]: session },
      allIds: appendUnique(state.sessions.allIds, session.id),
      agentIds,
      crossAgentIds,
      pinnedIds,
    },
  };
}

function incomingRevision(run: RunEntity): number | null {
  return Number.isSafeInteger(run.lifecycle_revision) ? (run.lifecycle_revision ?? null) : null;
}

function cursorIsStale(
  previous: { readonly cursor: number; readonly streamEpoch: string | null } | undefined,
  cursor: number | null,
  streamEpoch: string | null,
): boolean {
  return (
    cursor != null &&
    previous != null &&
    previous.streamEpoch === streamEpoch &&
    cursor <= previous.cursor
  );
}

function mergeRun(existing: RunEntity | undefined, incoming: RunEntity): RunEntity {
  const previousRevision = existing ? incomingRevision(existing) : null;
  const nextRevision = incomingRevision(incoming);
  if (
    existing &&
    previousRevision != null &&
    nextRevision != null &&
    nextRevision < previousRevision
  ) {
    return existing;
  }
  return existing ? { ...existing, ...incoming } : incoming;
}
function replaceRuns(
  state: CoreState,
  sessionId: string,
  incomingRuns: readonly RunEntity[],
): CoreState {
  const previousIds = state.runs.idsBySession[sessionId] ?? [];
  const byId: Record<string, RunEntity> = { ...state.runs.byId };
  for (const id of previousIds) delete byId[id];

  const ids: string[] = [];
  for (const incoming of incomingRuns) {
    const run = mergeRun(state.runs.byId[incoming.run_id], {
      ...incoming,
      session_id: incoming.session_id ?? sessionId,
    });
    byId[run.run_id] = run;
    if (!ids.includes(run.run_id)) ids.push(run.run_id);
  }

  return {
    ...state,
    runs: {
      ...state.runs,
      byId,
      allIds: Object.keys(byId),
      idsBySession: { ...state.runs.idsBySession, [sessionId]: ids },
      visibleSessionId: sessionId,
    },
  };
}

function clearRuns(state: CoreState, sessionId?: string): CoreState {
  if (!sessionId) {
    return {
      ...state,
      runs: {
        ...emptyTable<RunEntity>(),
        idsBySession: {},
        visibleSessionId: null,
        cursorById: {},
      },
    };
  }

  const byId: Record<string, RunEntity> = { ...state.runs.byId };
  const cursorById = { ...state.runs.cursorById };
  for (const id of state.runs.idsBySession[sessionId] ?? []) {
    delete byId[id];
    delete cursorById[id];
  }
  const idsBySession = { ...state.runs.idsBySession };
  delete idsBySession[sessionId];
  return {
    ...state,
    runs: {
      ...state.runs,
      byId,
      allIds: Object.keys(byId),
      idsBySession,
      visibleSessionId:
        state.runs.visibleSessionId === sessionId ? null : state.runs.visibleSessionId,
      cursorById,
    },
  };
}

function upsertRun(
  state: CoreState,
  incoming: RunEntity,
  cursor: number | null,
  streamEpoch: string | null,
): CoreState {
  const previousCursor = state.runs.cursorById[incoming.run_id];
  if (cursorIsStale(previousCursor, cursor, streamEpoch)) return state;

  const existing = state.runs.byId[incoming.run_id];
  const merged = mergeRun(existing, incoming);
  if (merged === existing && cursor == null) return state;

  const sessionId = merged.session_id ?? existing?.session_id;
  const idsBySession = { ...state.runs.idsBySession };
  if (sessionId) {
    idsBySession[sessionId] = [
      merged.run_id,
      ...(idsBySession[sessionId] ?? []).filter((id) => id !== merged.run_id),
    ];
  }

  const cursorById =
    cursor == null
      ? state.runs.cursorById
      : {
          ...state.runs.cursorById,
          [merged.run_id]: { cursor, streamEpoch },
        };

  return {
    ...state,
    runs: {
      ...state.runs,
      byId: { ...state.runs.byId, [merged.run_id]: merged },
      allIds: appendUnique(state.runs.allIds, merged.run_id),
      idsBySession,
      cursorById,
    },
  };
}

function updateSessionActivityFlag(
  sessions: CoreState["sessions"],
  sessionId: string,
  hasActiveRun: boolean,
): CoreState["sessions"] {
  const session = sessions.byId[sessionId];
  if (!session || session.has_active_run === hasActiveRun) return sessions;
  return {
    ...sessions,
    byId: {
      ...sessions.byId,
      [sessionId]: { ...session, has_active_run: hasActiveRun },
    },
  };
}
function activitySnapshot(
  state: CoreState,
  sessions: readonly SessionEntity[],
  cursor: number | null,
  streamEpoch: string | null,
): CoreState {
  const bySessionId: Record<string, ActivityRecord> = {};
  let nextSessions = state.sessions;
  for (const session of sessions) {
    bySessionId[session.id] = {
      sessionId: session.id,
      runId: null,
      hasActiveRun: session.has_active_run,
      cursor,
      streamEpoch,
    };
    nextSessions = updateSessionActivityFlag(nextSessions, session.id, session.has_active_run);
  }
  return {
    ...state,
    sessions: nextSessions,
    activity: {
      bySessionId,
      appliedCursor: cursor,
      streamEpoch,
    },
  };
}

function applyActivityTransition(state: CoreState, event: ActivityTransition): CoreState {
  const current = state.activity.bySessionId[event.data.session_id];
  const epochChanged =
    event.streamEpoch != null && event.streamEpoch !== state.activity.streamEpoch;
  const globalCursorIsStale =
    event.cursor != null &&
    state.activity.appliedCursor != null &&
    state.activity.streamEpoch === event.streamEpoch &&
    event.cursor <= state.activity.appliedCursor;
  if (
    globalCursorIsStale ||
    cursorIsStale(
      current?.cursor == null
        ? undefined
        : { cursor: current.cursor, streamEpoch: current.streamEpoch },
      event.cursor,
      event.streamEpoch,
    )
  ) {
    return state;
  }

  const hasActiveRun = event.data.has_active_run;
  const record: ActivityRecord = {
    sessionId: event.data.session_id,
    runId: hasActiveRun && event.type === "session_activity_started" ? event.data.run_id : null,
    hasActiveRun,
    cursor: event.cursor,
    streamEpoch: event.streamEpoch,
  };

  return {
    ...state,
    sessions: updateSessionActivityFlag(state.sessions, event.data.session_id, hasActiveRun),
    activity: {
      bySessionId: {
        ...state.activity.bySessionId,
        [event.data.session_id]: record,
      },
      appliedCursor:
        event.cursor == null
          ? epochChanged
            ? null
            : state.activity.appliedCursor
          : epochChanged
            ? event.cursor
            : Math.max(state.activity.appliedCursor ?? event.cursor, event.cursor),
      streamEpoch: event.streamEpoch ?? state.activity.streamEpoch,
    },
  };
}

function commitActivityReconciliation(
  state: CoreState,
  token: number,
  sessions: readonly SessionEntity[],
): CoreState {
  const reconciliation = state.activityReconciliation;
  if (!reconciliation || reconciliation.token !== token || reconciliation.overflowed) {
    return state;
  }

  let next = activitySnapshot(
    { ...state, activityReconciliation: null },
    sessions,
    reconciliation.replayCeiling,
    reconciliation.streamEpoch,
  );
  for (const event of reconciliation.buffered) {
    if (
      reconciliation.streamEpoch === event.streamEpoch &&
      reconciliation.replayCeiling != null &&
      event.cursor != null &&
      event.cursor <= reconciliation.replayCeiling
    ) {
      continue;
    }
    next = applyActivityTransition(next, event);
  }
  return next;
}

export function reduceCoreState(state: CoreState, action: CoreAction): CoreState {
  switch (action.type) {
    case "agents/replaced":
      return replaceAgents(state, action.agents);
    case "sessions/replaced":
      return replaceSessions(state, action.agentSessions, action.crossAgentSessions);
    case "session/upserted":
      return upsertSession(state, action.session, action.scope);
    case "scoped/reset":
      return {
        ...state,
        sessions: {
          ...emptyTable<SessionEntity>(),
          agentIds: [],
          crossAgentIds: [],
          pinnedIds: [],
        },
        runs: {
          ...emptyTable<RunEntity>(),
          idsBySession: {},
          visibleSessionId: null,
          cursorById: {},
        },
        activity: { bySessionId: {}, appliedCursor: null, streamEpoch: null },
        activityReconciliation: null,
      };
    case "runs/replaced":
      return replaceRuns(state, action.sessionId, action.runs);
    case "runs/cleared":
      return clearRuns(state, action.sessionId);
    case "run/upserted":
      return upsertRun(state, action.run, action.cursor, action.streamEpoch);
    case "activity/snapshot":
      return activitySnapshot(state, action.sessions, action.cursor, action.streamEpoch);
    case "activity/event":
      if (state.activityReconciliation) {
        const reconciliation = state.activityReconciliation;
        if (reconciliation.buffered.length >= MAX_BUFFERED_ACTIVITY_EVENTS) {
          return {
            ...state,
            activityReconciliation: { ...reconciliation, overflowed: true },
          };
        }
        return {
          ...state,
          activityReconciliation: {
            ...reconciliation,
            buffered: [...reconciliation.buffered, action.event],
          },
        };
      }
      return applyActivityTransition(state, action.event);
    case "activity/reconciliation-began":
      return {
        ...state,
        activityReconciliation: {
          token: action.token,
          replayCeiling: action.replayCeiling,
          streamEpoch: action.streamEpoch,
          buffered: [],
          overflowed: false,
        },
      };
    case "activity/reconciliation-committed":
      return commitActivityReconciliation(state, action.token, action.sessions);
    case "activity/reconciliation-aborted":
      return state.activityReconciliation?.token === action.token
        ? { ...state, activityReconciliation: null }
        : state;
  }
}
const stateSignal = signal<CoreState>(createInitialCoreState());
const agentsSignal = computed(() =>
  stateSignal.value.agents.allIds.flatMap((id) => {
    const entity = stateSignal.value.agents.byId[id];
    return entity ? [entity] : [];
  }),
);
const agentSessionsSignal = computed(() => {
  const visibleIds = [
    ...new Set([...stateSignal.value.sessions.agentIds, ...stateSignal.value.sessions.pinnedIds]),
  ];
  return visibleIds.flatMap((id) => {
    const entity = stateSignal.value.sessions.byId[id];
    return entity ? [entity] : [];
  });
});
const crossAgentSessionsSignal = computed(() =>
  stateSignal.value.sessions.crossAgentIds.flatMap((id) => {
    const entity = stateSignal.value.sessions.byId[id];
    return entity ? [entity] : [];
  }),
);
const runsSignal = computed(() => {
  const sessionId = stateSignal.value.runs.visibleSessionId;
  if (!sessionId) return [];
  return (stateSignal.value.runs.idsBySession[sessionId] ?? []).flatMap((id) => {
    const entity = stateSignal.value.runs.byId[id];
    return entity ? [entity] : [];
  });
});
const activeRunIdSignal = computed(() => {
  const visible = runsSignal.value;
  return (
    visible.find((run) => run.status === "running")?.run_id ??
    visible.find((run) => run.status === "queued")?.run_id ??
    null
  );
});
const backgroundRunsSignal = computed(() => {
  const result: Record<string, { runId: string | null; finished: false }> = {};
  for (const activity of Object.values(stateSignal.value.activity.bySessionId)) {
    if (activity.hasActiveRun) {
      result[activity.sessionId] = { runId: activity.runId, finished: false };
    }
  }
  return result;
});

function dispatch(action: CoreAction): void {
  stateSignal.value = reduceCoreState(stateSignal.value, action);
}

let reconciliationToken = 0;

export interface CoreStateBridge {
  readonly version: 1;
  readonly state: ReadonlySignal<CoreState>;
  readonly agents: ReadonlySignal<readonly AgentEntity[]>;
  readonly agentSessions: ReadonlySignal<readonly SessionEntity[]>;
  readonly crossAgentSessions: ReadonlySignal<readonly SessionEntity[]>;
  readonly runs: ReadonlySignal<readonly RunEntity[]>;
  readonly activeRunId: ReadonlySignal<string | null>;
  readonly backgroundRuns: ReadonlySignal<
    Readonly<Record<string, { readonly runId: string | null; readonly finished: false }>>
  >;
  replaceAgents(agents: readonly AgentEntity[]): void;
  replaceSessionScopes(
    agentSessions: readonly SessionEntity[],
    crossAgentSessions: readonly SessionEntity[],
  ): void;
  upsertSession(session: SessionEntity, scope: "agent" | "cross" | "pinned"): void;
  resetScopedState(): void;
  replaceRuns(sessionId: string, runs: readonly RunEntity[]): void;
  clearRuns(sessionId?: string): void;
  upsertRun(run: RunEntity, cursor?: number | null, streamEpoch?: string | null): void;
  setRunStatus(
    runId: string,
    status: RunStatus,
    options?: {
      readonly sessionId?: string;
      readonly cursor?: number | null;
      readonly streamEpoch?: string | null;
    },
  ): void;
  replaceActivitySnapshot(
    sessions: readonly SessionEntity[],
    cursor?: number | null,
    streamEpoch?: string | null,
  ): void;
  applyActivityEvent(
    type: ActivityTransition["type"],
    data: SessionActivity,
    cursor?: number | null,
    streamEpoch?: string | null,
  ): void;
  beginActivityReconciliation(replayCeiling?: number | null, streamEpoch?: string | null): number;
  commitActivityReconciliation(token: number, sessions: readonly SessionEntity[]): boolean;
  abortActivityReconciliation(token: number): void;
}

export const coreStateBridge: CoreStateBridge = {
  version: 1,
  state: stateSignal,
  agents: agentsSignal,
  agentSessions: agentSessionsSignal,
  crossAgentSessions: crossAgentSessionsSignal,
  runs: runsSignal,
  activeRunId: activeRunIdSignal,
  backgroundRuns: backgroundRunsSignal,
  replaceAgents: (agents) => {
    dispatch({ type: "agents/replaced", agents });
  },
  replaceSessionScopes: (agentSessions, crossAgentSessions) => {
    dispatch({ type: "sessions/replaced", agentSessions, crossAgentSessions });
  },
  upsertSession: (session, scope) => {
    dispatch({ type: "session/upserted", session, scope });
  },
  resetScopedState: () => {
    dispatch({ type: "scoped/reset" });
  },
  replaceRuns: (sessionId, runs) => {
    dispatch({ type: "runs/replaced", sessionId, runs });
  },
  clearRuns: (sessionId) => {
    dispatch({ type: "runs/cleared", sessionId });
  },
  upsertRun: (run, cursor = null, streamEpoch = null) => {
    dispatch({ type: "run/upserted", run, cursor, streamEpoch });
  },
  setRunStatus: (runId, status, options) => {
    dispatch({
      type: "run/upserted",
      run: {
        run_id: runId,
        session_id: options?.sessionId,
        status,
      },
      cursor: options?.cursor ?? null,
      streamEpoch: options?.streamEpoch ?? null,
    });
  },
  replaceActivitySnapshot: (sessions, cursor = null, streamEpoch = null) => {
    dispatch({ type: "activity/snapshot", sessions, cursor, streamEpoch });
  },
  applyActivityEvent: (type, data, cursor = null, streamEpoch = null) => {
    dispatch({
      type: "activity/event",
      event: { type, data, cursor, streamEpoch },
    });
  },
  beginActivityReconciliation: (replayCeiling = null, streamEpoch = null) => {
    const token = ++reconciliationToken;
    dispatch({
      type: "activity/reconciliation-began",
      token,
      replayCeiling,
      streamEpoch,
    });
    return token;
  },
  commitActivityReconciliation: (token, sessions) => {
    const reconciliation = stateSignal.peek().activityReconciliation;
    if (!reconciliation || reconciliation.token !== token || reconciliation.overflowed) {
      if (reconciliation?.token === token) {
        dispatch({ type: "activity/reconciliation-aborted", token });
      }
      return false;
    }
    dispatch({ type: "activity/reconciliation-committed", token, sessions });
    return true;
  },
  abortActivityReconciliation: (token) => {
    dispatch({ type: "activity/reconciliation-aborted", token });
  },
};
