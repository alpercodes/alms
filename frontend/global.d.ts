import type { AlmsContractBridge } from "./bridge";
import type { AlmsStateBridge } from "./state/bridge";

declare global {
  var __almsContracts: AlmsContractBridge | undefined;
  var __almsState: AlmsStateBridge | undefined;
}

export {};
