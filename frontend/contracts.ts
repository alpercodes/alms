import { z, type ZodType } from "zod";

const uuidSchema = z.string().uuid();
const nullableCursorSchema = z.number().int().nonnegative().nullable();
const objectPayloadSchema = z.record(z.string(), z.unknown());

export const streamStateSchema = z
  .object({
    stream_epoch: uuidSchema,
    retained_from: nullableCursorSchema,
    newest: nullableCursorSchema,
    replay_gap: z.boolean(),
    epoch_mismatch: z.boolean(),
    requires_reconciliation: z.boolean(),
  })
  .passthrough();

export const sessionActivitySchema = z
  .object({
    session_id: uuidSchema,
    run_id: uuidSchema,
    agent_id: uuidSchema,
    has_active_run: z.boolean(),
    ts: z.string().min(1),
  })
  .passthrough();

export const sessionsSnapshotSchema = z
  .object({
    sessions: z.array(
      z
        .object({
          id: uuidSchema,
          session_type: z.string().min(1),
          has_active_run: z.boolean(),
        })
        .passthrough(),
    ),
  })
  .passthrough();

export const agentsSnapshotSchema = z
  .object({
    agents: z.array(
      z
        .object({
          id: uuidSchema,
          name: z.string().min(1),
          is_default: z.boolean(),
        })
        .passthrough(),
    ),
  })
  .passthrough();

const sseSchemas = new Map<string, ZodType>([
  ["stream_state", streamStateSchema],
  ["session_activity_started", sessionActivitySchema],
  ["session_activity_ended", sessionActivitySchema],
]);

export class ContractViolation extends Error {
  constructor(
    readonly boundary: string,
    readonly issues: z.core.$ZodIssue[],
  ) {
    const summary = issues
      .slice(0, 3)
      .map((issue) => `${issue.path.join(".") || "<root>"}: ${issue.message}`)
      .join("; ");
    super(`Invalid ${boundary} payload: ${summary}`);
    this.name = "ContractViolation";
  }
}

function parseAtBoundary<T>(boundary: string, schema: ZodType<T>, input: unknown): T {
  const result = schema.safeParse(input);
  if (!result.success) {
    throw new ContractViolation(boundary, result.error.issues);
  }
  return result.data;
}

export function parseSsePayload(type: string, input: unknown): unknown {
  const schema = sseSchemas.get(type) ?? objectPayloadSchema;
  return parseAtBoundary(`SSE ${type}`, schema, input);
}

export function parseApiResponse(path: string, method: string, input: unknown): unknown {
  const pathname = new URL(path, "http://alms.local").pathname;
  const normalizedMethod = method.toUpperCase();
  if (normalizedMethod === "GET" && pathname === "/sessions") {
    return parseAtBoundary("GET /sessions", sessionsSnapshotSchema, input);
  }
  if (normalizedMethod === "GET" && pathname === "/agents") {
    return parseAtBoundary("GET /agents", agentsSnapshotSchema, input);
  }
  return parseAtBoundary(`${normalizedMethod} ${pathname}`, objectPayloadSchema, input);
}
