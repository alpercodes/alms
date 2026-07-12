import type { AlmsContractBridge } from "./bridge";

declare global {
  var __almsContracts: AlmsContractBridge | undefined;
}

export {};
