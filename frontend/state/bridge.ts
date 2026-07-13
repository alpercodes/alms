import { coreStateBridge, type CoreStateBridge } from "./core-store";

export type AlmsStateBridge = CoreStateBridge;

export function installStateBridge(): AlmsStateBridge {
  if (globalThis.__almsState) {
    return globalThis.__almsState;
  }
  globalThis.__almsState = coreStateBridge;
  return coreStateBridge;
}
