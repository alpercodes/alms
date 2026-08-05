import type { ReadonlySignal, Signal } from "@preact/signals";
import type { AgentEntity } from "../../../../../frontend/state/core-store";

export declare const agents: ReadonlySignal<readonly AgentEntity[]>;
export declare const activeAgentId: Signal<string | null>;
export declare const activeAgent: ReadonlySignal<AgentEntity | null>;
export declare function replaceAgents(nextAgents: readonly AgentEntity[]): void;
