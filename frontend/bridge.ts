import { showContractError } from "./contract-error-banner";
import { ContractViolation, parseApiResponse, parseSsePayload } from "./contracts";

export interface AlmsContractBridge {
  readonly version: 1;
  parseApiResponse(path: string, method: string, input: unknown): unknown;
  parseApiJsonResponse(path: string, method: string, input: string): unknown;
  parseSsePayload(type: string, input: unknown): unknown;
  parseSseJsonPayload(type: string, input: string): unknown;
}

function guard<T>(boundary: string, operation: () => T): T {
  try {
    return operation();
  } catch (error) {
    const violation =
      error instanceof ContractViolation
        ? error
        : new ContractViolation(boundary, [
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
    parseApiResponse: (path, method, input) =>
      guard(`${method.toUpperCase()} ${path}`, () => parseApiResponse(path, method, input)),
    parseApiJsonResponse: (path, method, input) =>
      guard(`${method.toUpperCase()} ${path} JSON`, () =>
        parseApiResponse(path, method, JSON.parse(input) as unknown),
      ),
    parseSsePayload: (type, input) => guard(`SSE ${type}`, () => parseSsePayload(type, input)),
    parseSseJsonPayload: (type, input) =>
      guard(`SSE ${type} JSON`, () => parseSsePayload(type, JSON.parse(input) as unknown)),
  };
  globalThis.__almsContracts = bridge;
  return bridge;
}
