import { showContractError } from "./contract-error-banner";
import { ContractViolation, parseApiResponse, parseSsePayload } from "./contracts";

export interface AlmsContractBridge {
  readonly version: 1;
  parseApiResponse(path: string, method: string, input: unknown): unknown;
  parseSsePayload(type: string, input: unknown): unknown;
}

function guard<T>(operation: () => T): T {
  try {
    return operation();
  } catch (error) {
    const violation =
      error instanceof ContractViolation
        ? error
        : new ContractViolation("unknown", [
            {
              code: "custom",
              path: [],
              message: error instanceof Error ? error.message : String(error),
              input: error,
            },
          ]);
    console.error("[contract-boundary]", violation);
    showContractError(violation.message);
    throw violation;
  }
}

export function installContractBridge(): AlmsContractBridge {
  const bridge: AlmsContractBridge = {
    version: 1,
    parseApiResponse: (path, method, input) => guard(() => parseApiResponse(path, method, input)),
    parseSsePayload: (type, input) => guard(() => parseSsePayload(type, input)),
  };
  globalThis.__almsContracts = bridge;
  return bridge;
}
