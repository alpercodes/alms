import { batch, computed, signal, type ReadonlySignal } from "@preact/signals";

import type { Agent, Job, Session, SessionActivity } from "../contracts";

export type RunStatus = "queued" | "running" | "completed" | "failed" | "cancelled";
export type AgentEntity = Pick<Agent, "id" | "name" | "is_default"> &
  Partial<Agent> &
  Record<string, unknown>;
export type SessionEntity = Session;
export type MessageEntity = {
  id: string;
  type: string;
  [key: string]: unknown;
};
export type JobEntity = Job & Record<string, unknown>;
export type RunEntity = {
  run_id: string;
  session_id?: string;
  status: RunStatus;
  lifecycle_revision?: number;
  [key: string]: unknown;
};

export interface PendingMessage {
  readonly messageId: string;
  readonly text: string;
  readonly runId: string | null;
  readonly ts: string;
}

export interface MessageCorrelation {
  readonly messageId?: string;
  readonly runId?: string;
}

type OptimisticJob =
  { readonly kind: "create" } | { readonly kind: "cancel"; readonly previous: JobEntity };

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
  readonly messages: EntityTable<MessageEntity> & {
    readonly idsBySession: Readonly<Record<string, readonly string[]>>;
    readonly visibleSessionId: string | null;
    readonly pendingBySession: Readonly<Record<string, readonly PendingMessage[]>>;
  };
  readonly jobs: EntityTable<JobEntity> & {
    readonly optimisticById: Readonly<Record<string, OptimisticJob>>;
    readonly mutationGeneration: number;
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
      readonly type: "messages/replaced";
      readonly sessionId: string;
      readonly messages: readonly MessageEntity[];
    }
  | {
      readonly type: "messages/updated";
      readonly sessionId: string;
      readonly messages: readonly MessageEntity[];
    }
  | { readonly type: "messages/unbound" }
  | { readonly type: "messages/cleared"; readonly sessionId: string }
  | {
      readonly type: "message/optimistic-began";
      readonly sessionId: string;
      readonly message: MessageEntity;
      readonly pending: PendingMessage;
      readonly companions: readonly MessageEntity[];
    }
  | {
      readonly type: "message/optimistic-linked";
      readonly sessionId: string;
      readonly messageId: string;
      readonly runId: string;
    }
  | {
      readonly type: "message/optimistic-confirmed";
      readonly sessionId: string;
      readonly correlation: MessageCorrelation;
    }
  | {
      readonly type: "message/optimistic-rolled-back";
      readonly sessionId: string;
      readonly correlation: MessageCorrelation;
    }
  | {
      readonly type: "jobs/replaced";
      readonly jobs: readonly JobEntity[];
      readonly mutationGeneration: number;
    }
  | { readonly type: "job/optimistic-created"; readonly job: JobEntity }
  | {
      readonly type: "job/optimistic-create-confirmed";
      readonly optimisticId: string;
      readonly job: JobEntity;
    }
  | { readonly type: "job/optimistic-create-rolled-back"; readonly optimisticId: string }
  | { readonly type: "job/optimistic-cancelled"; readonly jobId: string }
  | {
      readonly type: "job/optimistic-cancel-confirmed";
      readonly jobId: string;
      readonly job: JobEntity;
    }
  | { readonly type: "job/optimistic-cancel-rolled-back"; readonly jobId: string }
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
    messages: {
      ...emptyTable<MessageEntity>(),
      idsBySession: {},
      visibleSessionId: null,
      pendingBySession: {},
    },
    jobs: {
      ...emptyTable<JobEntity>(),
      optimisticById: {},
      mutationGeneration: 0,
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

function writeMessages(
  state: CoreState,
  sessionId: string,
  incomingMessages: readonly MessageEntity[],
  bindVisible: boolean,
): CoreState {
  const previousIds = state.messages.idsBySession[sessionId] ?? [];
  const byId: Record<string, MessageEntity> = { ...state.messages.byId };
  const allIds = new Set(state.messages.allIds);
  for (const id of previousIds) {
    delete byId[id];
    allIds.delete(id);
  }

  const ids: string[] = [];
  const idSet = new Set<string>();
  for (const message of incomingMessages) {
    byId[message.id] = message;
    if (!idSet.has(message.id)) {
      idSet.add(message.id);
      ids.push(message.id);
      allIds.add(message.id);
    }
  }

  for (const pending of state.messages.pendingBySession[sessionId] ?? []) {
    const pendingMessage = state.messages.byId[pending.messageId];
    if (pendingMessage && !idSet.has(pendingMessage.id)) {
      byId[pendingMessage.id] = pendingMessage;
      idSet.add(pendingMessage.id);
      ids.push(pendingMessage.id);
      allIds.add(pendingMessage.id);
    }
  }

  return {
    ...state,
    messages: {
      ...state.messages,
      byId,
      allIds: [...allIds],
      idsBySession: { ...state.messages.idsBySession, [sessionId]: ids },
      visibleSessionId: bindVisible ? sessionId : state.messages.visibleSessionId,
    },
  };
}

function clearMessages(state: CoreState, sessionId: string): CoreState {
  const byId: Record<string, MessageEntity> = { ...state.messages.byId };
  for (const id of state.messages.idsBySession[sessionId] ?? []) delete byId[id];
  const idsBySession = { ...state.messages.idsBySession };
  delete idsBySession[sessionId];
  const pendingBySession = { ...state.messages.pendingBySession };
  delete pendingBySession[sessionId];
  return {
    ...state,
    messages: {
      ...state.messages,
      byId,
      allIds: Object.keys(byId),
      idsBySession,
      visibleSessionId:
        state.messages.visibleSessionId === sessionId ? null : state.messages.visibleSessionId,
      pendingBySession,
    },
  };
}

function beginOptimisticMessage(
  state: CoreState,
  sessionId: string,
  message: MessageEntity,
  pending: PendingMessage,
  companions: readonly MessageEntity[],
): CoreState {
  const current = (state.messages.idsBySession[sessionId] ?? []).flatMap((id) => {
    const message = state.messages.byId[id];
    return message ? [message] : [];
  });
  const withMessages = writeMessages(state, sessionId, [...current, message, ...companions], false);
  return {
    ...withMessages,
    messages: {
      ...withMessages.messages,
      pendingBySession: {
        ...withMessages.messages.pendingBySession,
        [sessionId]: [
          ...(withMessages.messages.pendingBySession[sessionId] ?? []).filter(
            (entry) => entry.messageId !== pending.messageId,
          ),
          pending,
        ],
      },
    },
  };
}

function linkOptimisticMessage(
  state: CoreState,
  sessionId: string,
  messageId: string,
  runId: string,
): CoreState {
  const pending = state.messages.pendingBySession[sessionId] ?? [];
  const index = pending.findIndex((entry) => entry.messageId === messageId);
  if (index < 0) return state;
  const linked = [...pending];
  linked[index] = { ...linked[index], runId };
  const linkedState: CoreState = {
    ...state,
    messages: {
      ...state.messages,
      pendingBySession: {
        ...state.messages.pendingBySession,
        [sessionId]: linked,
      },
    },
  };

  const runStatus = linkedState.runs.byId[runId]?.status;
  if (runStatus === "completed" || runStatus === "failed" || runStatus === "cancelled") {
    return settleOptimisticMessage(linkedState, sessionId, { runId }, "confirmed");
  }
  return linkedState;
}

function persistedMessageRunId(message: MessageEntity | undefined): string | null {
  if (!message || typeof message.metadata !== "object" || message.metadata == null) return null;
  const runId = (message.metadata as Record<string, unknown>).run_id;
  return typeof runId === "string" ? runId : null;
}

function settleOptimisticMessage(
  state: CoreState,
  sessionId: string,
  correlation: MessageCorrelation,
  outcome: "confirmed" | "rolled-back",
): CoreState {
  const pending = state.messages.pendingBySession[sessionId] ?? [];
  const index = pending.findIndex(
    (entry) =>
      (correlation.messageId != null && entry.messageId === correlation.messageId) ||
      (correlation.runId != null && entry.runId === correlation.runId),
  );
  if (index < 0) return state;
  const settled = pending[index];
  const pendingBySession = { ...state.messages.pendingBySession };
  const remaining = pending.filter((_, entryIndex) => entryIndex !== index);
  if (remaining.length > 0) pendingBySession[sessionId] = remaining;
  else delete pendingBySession[sessionId];
  const existing = state.messages.byId[settled.messageId];
  const authoritativeId =
    outcome === "confirmed" && settled.runId
      ? (state.messages.idsBySession[sessionId] ?? []).find(
          (id) =>
            id !== settled.messageId &&
            persistedMessageRunId(state.messages.byId[id]) === settled.runId,
        )
      : undefined;

  let byId = state.messages.byId;
  let allIds = state.messages.allIds;
  let idsBySession = state.messages.idsBySession;
  if (authoritativeId && existing) {
    const withoutOptimistic: Record<string, MessageEntity> = { ...state.messages.byId };
    delete withoutOptimistic[existing.id];
    byId = withoutOptimistic;
    allIds = state.messages.allIds.filter((id) => id !== existing.id);
    idsBySession = {
      ...state.messages.idsBySession,
      [sessionId]: (state.messages.idsBySession[sessionId] ?? []).filter(
        (id) => id !== existing.id,
      ),
    };
  } else if (existing) {
    byId = {
      ...state.messages.byId,
      [existing.id]: {
        ...existing,
        optimistic: false,
        ...(outcome === "rolled-back" ? { failed: true } : {}),
      },
    };
  }
  return {
    ...state,
    messages: { ...state.messages, byId, allIds, idsBySession, pendingBySession },
  };
}

function replaceJobs(
  state: CoreState,
  incomingJobs: readonly JobEntity[],
  mutationGeneration: number,
): CoreState {
  if (mutationGeneration !== state.jobs.mutationGeneration) return state;

  const byId: Record<string, JobEntity> = {};
  const allIds = uniqueIds(incomingJobs, (job) => job.id);
  const optimisticById = { ...state.jobs.optimisticById };
  for (const job of incomingJobs) byId[job.id] = job;

  for (const [id, optimistic] of Object.entries(state.jobs.optimisticById)) {
    const current = state.jobs.byId[id];
    if (optimistic.kind === "create" && current) {
      byId[id] = current;
      if (!allIds.includes(id)) allIds.unshift(id);
    } else if (optimistic.kind === "cancel") {
      const snapshot = byId[id] ?? optimistic.previous;
      optimisticById[id] = { kind: "cancel", previous: snapshot };
      byId[id] = { ...snapshot, status: "cancelled", optimistic: true };
      if (!allIds.includes(id)) allIds.push(id);
    }
  }

  return {
    ...state,
    jobs: { byId, allIds, optimisticById, mutationGeneration: state.jobs.mutationGeneration },
  };
}

function createOptimisticJob(state: CoreState, job: JobEntity): CoreState {
  return {
    ...state,
    jobs: {
      byId: { ...state.jobs.byId, [job.id]: { ...job, optimistic: true } },
      allIds: [job.id, ...state.jobs.allIds.filter((id) => id !== job.id)],
      optimisticById: { ...state.jobs.optimisticById, [job.id]: { kind: "create" } },
      mutationGeneration: state.jobs.mutationGeneration + 1,
    },
  };
}

function confirmOptimisticJobCreate(
  state: CoreState,
  optimisticId: string,
  job: JobEntity,
): CoreState {
  if (state.jobs.optimisticById[optimisticId]?.kind !== "create") return state;
  const byId = { ...state.jobs.byId };
  delete byId[optimisticId];
  byId[job.id] = job;
  const optimisticById = { ...state.jobs.optimisticById };
  delete optimisticById[optimisticId];
  return {
    ...state,
    jobs: {
      byId,
      allIds: [...new Set(state.jobs.allIds.map((id) => (id === optimisticId ? job.id : id)))],
      optimisticById,
      mutationGeneration: state.jobs.mutationGeneration + 1,
    },
  };
}

function rollbackOptimisticJobCreate(state: CoreState, optimisticId: string): CoreState {
  if (state.jobs.optimisticById[optimisticId]?.kind !== "create") return state;
  const byId = { ...state.jobs.byId };
  delete byId[optimisticId];
  const optimisticById = { ...state.jobs.optimisticById };
  delete optimisticById[optimisticId];
  return {
    ...state,
    jobs: {
      byId,
      allIds: state.jobs.allIds.filter((id) => id !== optimisticId),
      optimisticById,
      mutationGeneration: state.jobs.mutationGeneration + 1,
    },
  };
}

function cancelOptimisticJob(state: CoreState, jobId: string): CoreState {
  const previous = state.jobs.byId[jobId];
  if (!previous || state.jobs.optimisticById[jobId]) return state;
  return {
    ...state,
    jobs: {
      ...state.jobs,
      byId: {
        ...state.jobs.byId,
        [jobId]: { ...previous, status: "cancelled", optimistic: true },
      },
      optimisticById: {
        ...state.jobs.optimisticById,
        [jobId]: { kind: "cancel", previous },
      },
      mutationGeneration: state.jobs.mutationGeneration + 1,
    },
  };
}

function settleOptimisticJobCancel(
  state: CoreState,
  jobId: string,
  authoritativeJob: JobEntity | null,
  outcome: "confirmed" | "rolled-back",
): CoreState {
  const optimistic = state.jobs.optimisticById[jobId];
  if (optimistic?.kind !== "cancel") return state;
  const optimisticById = { ...state.jobs.optimisticById };
  delete optimisticById[jobId];
  const current = state.jobs.byId[jobId];
  let job = optimistic.previous;
  if (outcome === "confirmed" && (authoritativeJob || current)) {
    job = { ...(authoritativeJob ?? current) };
    delete job.optimistic;
  }
  return {
    ...state,
    jobs: {
      ...state.jobs,
      byId: { ...state.jobs.byId, [jobId]: job },
      optimisticById,
      mutationGeneration: state.jobs.mutationGeneration + 1,
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
        messages: {
          ...emptyTable<MessageEntity>(),
          idsBySession: {},
          visibleSessionId: null,
          pendingBySession: {},
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
    case "messages/replaced":
      return writeMessages(state, action.sessionId, action.messages, true);
    case "messages/updated":
      return writeMessages(state, action.sessionId, action.messages, false);
    case "messages/unbound":
      return {
        ...state,
        messages: { ...state.messages, visibleSessionId: null },
      };
    case "messages/cleared":
      return clearMessages(state, action.sessionId);
    case "message/optimistic-began":
      return beginOptimisticMessage(
        state,
        action.sessionId,
        action.message,
        action.pending,
        action.companions,
      );
    case "message/optimistic-linked":
      return linkOptimisticMessage(state, action.sessionId, action.messageId, action.runId);
    case "message/optimistic-confirmed":
      return settleOptimisticMessage(state, action.sessionId, action.correlation, "confirmed");
    case "message/optimistic-rolled-back":
      return settleOptimisticMessage(state, action.sessionId, action.correlation, "rolled-back");
    case "jobs/replaced":
      return replaceJobs(state, action.jobs, action.mutationGeneration);
    case "job/optimistic-created":
      return createOptimisticJob(state, action.job);
    case "job/optimistic-create-confirmed":
      return confirmOptimisticJobCreate(state, action.optimisticId, action.job);
    case "job/optimistic-create-rolled-back":
      return rollbackOptimisticJobCreate(state, action.optimisticId);
    case "job/optimistic-cancelled":
      return cancelOptimisticJob(state, action.jobId);
    case "job/optimistic-cancel-confirmed":
      return settleOptimisticJobCancel(state, action.jobId, action.job, "confirmed");
    case "job/optimistic-cancel-rolled-back":
      return settleOptimisticJobCancel(state, action.jobId, null, "rolled-back");
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
const initialCoreState = createInitialCoreState();
const stateSignal = signal<CoreState>(initialCoreState);
const agentsStateSignal = signal(initialCoreState.agents);
const sessionsStateSignal = signal(initialCoreState.sessions);
const runsStateSignal = signal(initialCoreState.runs);
const messagesStateSignal = signal(initialCoreState.messages);
const jobsStateSignal = signal(initialCoreState.jobs);
const activityStateSignal = signal(initialCoreState.activity);

const agentsSignal = computed(() =>
  agentsStateSignal.value.allIds.flatMap((id) => {
    const entity = agentsStateSignal.value.byId[id];
    return entity ? [entity] : [];
  }),
);
const agentSessionsSignal = computed(() => {
  const sessions = sessionsStateSignal.value;
  const visibleIds = [...new Set([...sessions.agentIds, ...sessions.pinnedIds])];
  return visibleIds.flatMap((id) => {
    const entity = sessions.byId[id];
    return entity ? [entity] : [];
  });
});
const crossAgentSessionsSignal = computed(() => {
  const sessions = sessionsStateSignal.value;
  return sessions.crossAgentIds.flatMap((id) => {
    const entity = sessions.byId[id];
    return entity ? [entity] : [];
  });
});
const runsSignal = computed(() => {
  const runs = runsStateSignal.value;
  const sessionId = runs.visibleSessionId;
  if (!sessionId) return [];
  return (runs.idsBySession[sessionId] ?? []).flatMap((id) => {
    const entity = runs.byId[id];
    return entity ? [entity] : [];
  });
});
const messagesSignal = computed(() => {
  const messages = messagesStateSignal.value;
  const sessionId = messages.visibleSessionId;
  if (!sessionId) return [];
  return (messages.idsBySession[sessionId] ?? []).flatMap((id) => {
    const entity = messages.byId[id];
    return entity ? [entity] : [];
  });
});
const messageSessionIdSignal = computed(() => messagesStateSignal.value.visibleSessionId);
const jobsSignal = computed(() => {
  const jobs = jobsStateSignal.value;
  return jobs.allIds.flatMap((id) => {
    const entity = jobs.byId[id];
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
  for (const activity of Object.values(activityStateSignal.value.bySessionId)) {
    if (activity.hasActiveRun) {
      result[activity.sessionId] = { runId: activity.runId, finished: false };
    }
  }
  return result;
});

function dispatch(action: CoreAction): void {
  const current = stateSignal.peek();
  const next = reduceCoreState(current, action);
  if (next === current) return;

  batch(() => {
    stateSignal.value = next;
    if (next.agents !== current.agents) agentsStateSignal.value = next.agents;
    if (next.sessions !== current.sessions) sessionsStateSignal.value = next.sessions;
    if (next.runs !== current.runs) runsStateSignal.value = next.runs;
    if (next.messages !== current.messages) messagesStateSignal.value = next.messages;
    if (next.jobs !== current.jobs) jobsStateSignal.value = next.jobs;
    if (next.activity !== current.activity) activityStateSignal.value = next.activity;
  });
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
  readonly messages: ReadonlySignal<readonly MessageEntity[]>;
  readonly messageSessionId: ReadonlySignal<string | null>;
  readonly jobs: ReadonlySignal<readonly JobEntity[]>;
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
  replaceMessages(sessionId: string, messages: readonly MessageEntity[]): void;
  unbindMessages(): void;
  clearMessages(sessionId: string): void;
  transformMessages(
    sessionId: string,
    transformer: (messages: readonly MessageEntity[]) => readonly MessageEntity[],
  ): void;
  beginOptimisticMessage(
    sessionId: string,
    message: MessageEntity,
    pending: PendingMessage,
    companions?: readonly MessageEntity[],
  ): void;
  linkOptimisticMessage(sessionId: string, messageId: string, runId: string): void;
  confirmOptimisticMessage(sessionId: string, correlation: MessageCorrelation): void;
  rollbackOptimisticMessage(sessionId: string, correlation: MessageCorrelation): void;
  getPendingMessages(sessionId: string): readonly PendingMessage[];
  getJobMutationGeneration(): number;
  replaceJobs(jobs: readonly JobEntity[], mutationGeneration: number): void;
  createOptimisticJob(job: JobEntity): void;
  confirmOptimisticJobCreate(optimisticId: string, job: JobEntity): void;
  rollbackOptimisticJobCreate(optimisticId: string): void;
  cancelOptimisticJob(jobId: string): void;
  confirmOptimisticJobCancel(jobId: string, job: JobEntity): void;
  rollbackOptimisticJobCancel(jobId: string): void;
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
  messages: messagesSignal,
  messageSessionId: messageSessionIdSignal,
  jobs: jobsSignal,
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
  replaceMessages: (sessionId, messages) => {
    dispatch({ type: "messages/replaced", sessionId, messages });
  },
  unbindMessages: () => {
    dispatch({ type: "messages/unbound" });
  },
  clearMessages: (sessionId) => {
    dispatch({ type: "messages/cleared", sessionId });
  },
  transformMessages: (sessionId, transformer) => {
    const state = stateSignal.peek();
    const current = (state.messages.idsBySession[sessionId] ?? []).flatMap((id) => {
      const message = state.messages.byId[id];
      return message ? [message] : [];
    });
    const messages = transformer(current);
    if (messages === current) return;
    dispatch({ type: "messages/updated", sessionId, messages });
  },
  beginOptimisticMessage: (sessionId, message, pending, companions = []) => {
    dispatch({ type: "message/optimistic-began", sessionId, message, pending, companions });
  },
  linkOptimisticMessage: (sessionId, messageId, runId) => {
    dispatch({ type: "message/optimistic-linked", sessionId, messageId, runId });
  },
  confirmOptimisticMessage: (sessionId, correlation) => {
    dispatch({ type: "message/optimistic-confirmed", sessionId, correlation });
  },
  rollbackOptimisticMessage: (sessionId, correlation) => {
    dispatch({ type: "message/optimistic-rolled-back", sessionId, correlation });
  },
  getPendingMessages: (sessionId) => stateSignal.peek().messages.pendingBySession[sessionId] ?? [],
  getJobMutationGeneration: () => stateSignal.peek().jobs.mutationGeneration,
  replaceJobs: (jobs, mutationGeneration) => {
    dispatch({ type: "jobs/replaced", jobs, mutationGeneration });
  },
  createOptimisticJob: (job) => {
    dispatch({ type: "job/optimistic-created", job });
  },
  confirmOptimisticJobCreate: (optimisticId, job) => {
    dispatch({ type: "job/optimistic-create-confirmed", optimisticId, job });
  },
  rollbackOptimisticJobCreate: (optimisticId) => {
    dispatch({ type: "job/optimistic-create-rolled-back", optimisticId });
  },
  cancelOptimisticJob: (jobId) => {
    dispatch({ type: "job/optimistic-cancelled", jobId });
  },
  confirmOptimisticJobCancel: (jobId, job) => {
    dispatch({ type: "job/optimistic-cancel-confirmed", jobId, job });
  },
  rollbackOptimisticJobCancel: (jobId) => {
    dispatch({ type: "job/optimistic-cancel-rolled-back", jobId });
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
