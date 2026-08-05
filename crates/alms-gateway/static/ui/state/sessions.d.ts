import type { ReadonlySignal, Signal } from "@preact/signals";
import type { SessionEntity } from "../../../../../frontend/state/core-store";

export declare const sessions: ReadonlySignal<readonly SessionEntity[]>;
export declare const crossAgentSessions: ReadonlySignal<readonly SessionEntity[]>;
export declare const activeSessionId: Signal<string | null>;
export declare const expandedAgentId: Signal<string | null>;
export declare const jobsGroupExpanded: Signal<boolean>;
export declare const activeSession: ReadonlySignal<SessionEntity | null>;
export declare const isDmSession: ReadonlySignal<boolean>;
export declare const isNotificationSession: ReadonlySignal<boolean>;
export declare const isInternalSession: ReadonlySignal<boolean>;
export declare const activeSessionOwnerName: ReadonlySignal<string | null>;
export declare const dmParticipants: ReadonlySignal<readonly string[] | null>;
export declare function replaceSessionScopes(
  agentSessions: readonly SessionEntity[],
  crossSessions: readonly SessionEntity[],
): void;
export declare function upsertSession(
  session: SessionEntity,
  scope?: "agent" | "cross" | "pinned",
): void;
export declare function resetScopedEntities(): void;
