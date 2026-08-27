import { z, type ZodType } from "zod";

const uuidSchema = z.string().uuid();
const stringSchema = z.string().min(1);
const timestampSchema = z.string().min(1);
const uintSchema = z.number().int().nonnegative();
const positiveUintSchema = z.number().int().positive();
const nullableCursorSchema = uintSchema.nullable();
const jsonSchema = z.json();
const objectPayloadSchema = z.record(z.string(), z.unknown());
const nullableObjectSchema = objectPayloadSchema.nullable();

const okSchema = z.literal(true);
const runStatusSchema = z.enum(["queued", "running", "completed", "failed", "cancelled"]);

const usageSchema = z
  .object({
    prompt_tokens: uintSchema,
    completion_tokens: uintSchema,
    reasoning_tokens: uintSchema.optional(),
    cache_creation_input_tokens: uintSchema.optional(),
    cache_read_input_tokens: uintSchema.optional(),
  })
  .passthrough();

export const agentSchema = z
  .object({
    id: uuidSchema,
    name: stringSchema,
    description: z.string(),
    has_telegram: z.boolean(),
    debug_mode: z.boolean(),
    is_default: z.boolean(),
    model: stringSchema.optional(),
    posture: stringSchema.optional(),
    provider: stringSchema.optional(),
    thinking_budget_tokens: uintSchema.optional(),
    reasoning_effort: z.enum(["minimal", "low", "medium", "high"]).optional(),
    gemini_thinking_budget: uintSchema.optional(),
    summary_provider: stringSchema.optional(),
    summary_model: stringSchema.optional(),
  })
  .passthrough();

export const sessionSchema = z
  .object({
    id: uuidSchema,
    agent_id: uuidSchema,
    context_id: z.string(),
    session_type: z.enum(["chat", "dm", "notification", "job", "subagent", "telegram", "episodic"]),
    has_active_run: z.boolean(),
    participants: z.tuple([stringSchema, stringSchema]).optional(),
    agent_name: stringSchema.optional(),
    parent_session_id: uuidSchema.nullable().optional(),
  })
  .passthrough()
  .superRefine((session, context) => {
    if (session.session_type === "dm" && !session.participants) {
      context.addIssue({
        code: "custom",
        path: ["participants"],
        message: "DM sessions require exactly two participants",
      });
    }
  });

const messageRoleSchema = z.enum(["user", "assistant", "tool", "system"]);
const textMessageSchema = z
  .object({
    role: messageRoleSchema,
    type: z.literal("text"),
    content: z.string(),
    timestamp: timestampSchema,
    metadata: objectPayloadSchema.optional(),
  })
  .passthrough();
const toolCallMessageSchema = z
  .object({
    role: messageRoleSchema,
    type: z.literal("tool_call"),
    tool: stringSchema,
    params: jsonSchema,
    timestamp: timestampSchema,
    metadata: nullableObjectSchema,
  })
  .passthrough();
const toolResultMessageSchema = z
  .object({
    role: messageRoleSchema,
    type: z.literal("tool_result"),
    tool_id: stringSchema,
    result: jsonSchema,
    ok: z.boolean(),
    timestamp: timestampSchema,
    metadata: objectPayloadSchema.optional(),
  })
  .passthrough();
const imageMessageSchema = z
  .object({
    role: messageRoleSchema,
    type: z.literal("image"),
    url: stringSchema,
    alt: z.string().nullable(),
    timestamp: timestampSchema,
  })
  .passthrough();
const messageSchema = z.discriminatedUnion("type", [
  textMessageSchema,
  toolCallMessageSchema,
  toolResultMessageSchema,
  imageMessageSchema,
]);

export const sessionRunSchema = z
  .object({
    run_id: uuidSchema,
    session_id: uuidSchema,
    agent_id: uuidSchema,
    status: runStatusSchema,
    lifecycle_revision: uintSchema,
    terminal_reason: z.string().optional(),
    response: z.string().optional(),
    error: z.string().optional(),
    started_at: timestampSchema.nullable(),
    ended_at: timestampSchema.nullable(),
    usage: usageSchema.nullable(),
    ts: timestampSchema,
    job_id: uuidSchema.nullable(),
    parent_run_id: uuidSchema.optional(),
    tool_call_count: uintSchema.optional(),
    queue_position: positiveUintSchema.optional(),
    resolved_config: objectPayloadSchema.optional(),
  })
  .passthrough();

export const agentRunSchema = z
  .object({
    run_id: uuidSchema,
    session_id: uuidSchema,
    status: runStatusSchema,
    session_type: stringSchema,
    trigger: stringSchema,
    context_id: z.string(),
    ts: timestampSchema,
    duration_ms: uintSchema.nullable().optional(),
    tool_call_count: uintSchema.nullable().optional(),
    usage: usageSchema.nullable().optional(),
  })
  .passthrough();

const jobScheduleSchema = z.discriminatedUnion("type", [
  z.object({ type: z.literal("once"), run_at: timestampSchema }).passthrough(),
  z.object({ type: z.literal("recurring"), cron: stringSchema }).passthrough(),
]);
export const jobSchema = z
  .object({
    id: uuidSchema,
    prompt: z.string(),
    schedule: jobScheduleSchema,
    status: z.enum(["pending", "active", "completed", "failed", "cancelled"]),
    next_run_at: timestampSchema.nullable(),
    last_run_at: timestampSchema.nullable(),
    terminal_reason: z
      .enum(["completed", "deadline_reached", "retry_exhausted", "operator_cancelled"])
      .optional(),
    retry_count: uintSchema.optional(),
    last_error: z.string().optional(),
  })
  .passthrough();

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
    ts: timestampSchema,
  })
  .passthrough();

export type Agent = z.infer<typeof agentSchema>;
export type Session = z.infer<typeof sessionSchema>;
export type SessionRun = z.infer<typeof sessionRunSchema>;
export type AgentRun = z.infer<typeof agentRunSchema>;
export type Job = z.infer<typeof jobSchema>;
export type SessionActivity = z.infer<typeof sessionActivitySchema>;

export const sessionsSnapshotSchema = z
  .object({
    sessions: z.array(sessionSchema),
  })
  .passthrough();

export const agentsSnapshotSchema = z
  .object({
    agents: z.array(agentSchema),
  })
  .passthrough();

const subagentActivitySchema = z
  .object({
    run_id: uuidSchema,
    source_agent: stringSchema,
    kind: z.enum(["reasoning", "writing", "tool_start", "tool_end"]),
    tool: stringSchema.optional(),
    tool_invocation_id: stringSchema.optional(),
    parent_tool_invocation_id: stringSchema.optional(),
  })
  .passthrough()
  .superRefine((event, context) => {
    if (event.kind === "tool_start" && !event.tool) {
      context.addIssue({ code: "custom", path: ["tool"], message: "tool_start requires tool" });
    }
    if ((event.kind === "tool_start" || event.kind === "tool_end") && !event.tool_invocation_id) {
      context.addIssue({
        code: "custom",
        path: ["tool_invocation_id"],
        message: `${event.kind} requires tool_invocation_id`,
      });
    }
  });

const runErrorSchema = z
  .object({
    run_id: uuidSchema,
    error: z.object({ code: stringSchema, message: stringSchema }).passthrough(),
  })
  .passthrough();

const sseSchemaEntries = [
  [
    "run_created",
    z
      .object({
        run_id: uuidSchema,
        session_id: uuidSchema,
        is_notification: z.boolean(),
        queued_behind: uintSchema,
        ts: timestampSchema,
        source: stringSchema.optional(),
      })
      .passthrough(),
  ],
  [
    "run_started",
    z
      .object({
        run_id: uuidSchema,
        session_id: uuidSchema,
        ts: timestampSchema,
        resolved_config: objectPayloadSchema.optional(),
      })
      .passthrough(),
  ],
  [
    "run_queue_position",
    z
      .object({
        run_id: uuidSchema,
        session_id: uuidSchema,
        agent_id: uuidSchema,
        position: positiveUintSchema,
        ts: timestampSchema,
      })
      .passthrough(),
  ],
  [
    "status",
    z
      .object({
        run_id: uuidSchema,
        phase: stringSchema,
        ts: timestampSchema,
        detail: z.string().optional(),
      })
      .passthrough(),
  ],
  [
    "token_delta",
    z
      .object({ run_id: uuidSchema, delta: z.string(), source_agent: stringSchema.optional() })
      .passthrough(),
  ],
  [
    "reasoning_delta",
    z
      .object({ run_id: uuidSchema, text: z.string(), source_agent: stringSchema.optional() })
      .passthrough(),
  ],
  [
    "stream_reset",
    z.object({ run_id: uuidSchema, source_agent: stringSchema.optional() }).passthrough(),
  ],
  [
    "tool_start",
    z
      .object({
        run_id: uuidSchema,
        tool_invocation_id: stringSchema,
        tool: stringSchema,
        params: jsonSchema,
        source_agent: stringSchema.optional(),
        task_id: stringSchema.optional(),
      })
      .passthrough(),
  ],
  [
    "tool_end",
    z
      .object({
        run_id: uuidSchema,
        tool_invocation_id: stringSchema,
        ok: z.boolean(),
        result: jsonSchema,
        source_agent: stringSchema.optional(),
        task_id: stringSchema.optional(),
      })
      .passthrough(),
  ],
  [
    "approval_required",
    z
      .object({
        run_id: uuidSchema,
        approval_id: uuidSchema,
        capability: stringSchema,
        request: jsonSchema,
        source_agent: stringSchema.optional(),
      })
      .passthrough(),
  ],
  ["subagent_activity", subagentActivitySchema],
  [
    "subagent_started",
    z
      .object({
        session_id: uuidSchema,
        tool_invocation_id: stringSchema,
        subagent_session_id: uuidSchema,
        ts: timestampSchema,
        subagent_name: stringSchema.optional(),
      })
      .passthrough(),
  ],
  [
    "subagent_completed",
    z
      .object({
        session_id: uuidSchema,
        status: stringSchema,
        summary: z.string(),
        subagent_session_id: uuidSchema,
        ts: timestampSchema,
        tool_invocation_id: stringSchema.optional(),
        subagent_name: stringSchema.optional(),
      })
      .passthrough(),
  ],
  [
    "job_completed",
    z
      .object({
        session_id: uuidSchema,
        job_name: stringSchema,
        status: stringSchema,
        summary: z.string(),
        run_id: uuidSchema,
        job_id: uuidSchema,
        job_session_id: stringSchema,
        truncated: z.boolean(),
        ts: timestampSchema,
        job_session_uuid: uuidSchema.optional(),
      })
      .passthrough(),
  ],
  [
    "dm_message",
    z
      .object({
        session_id: uuidSchema,
        from_agent: stringSchema,
        from_agent_id: stringSchema,
        message: z.string(),
        ts: timestampSchema,
      })
      .passthrough(),
  ],
  [
    "dm_conversation_ended",
    z
      .object({
        session_id: uuidSchema,
        ended_by: stringSchema,
        peer: stringSchema,
        reason: stringSchema,
        context_id: z.string(),
        ts: timestampSchema,
        suppress_banner: z.boolean().optional(),
        // #1258: failure text of an `errored` end. Present only on the
        // web-chat forward, where an interrupted DM end no longer spends an
        // LLM turn explaining itself.
        detail: z.string().optional(),
      })
      .passthrough(),
  ],
  [
    "dm_activity_started",
    z.object({ session_id: uuidSchema, peer: stringSchema, ts: timestampSchema }).passthrough(),
  ],
  [
    "dm_activity_status",
    z
      .object({
        session_id: uuidSchema,
        peer: stringSchema,
        phase: stringSchema,
        ts: timestampSchema,
        detail: z.string().optional(),
      })
      .passthrough(),
  ],
  [
    "dm_activity_ended",
    z.object({ session_id: uuidSchema, peer: stringSchema, ts: timestampSchema }).passthrough(),
  ],
  [
    "approval_resolved",
    z
      .object({
        run_id: uuidSchema,
        approval_id: uuidSchema,
        decision: stringSchema,
        ts: timestampSchema,
      })
      .passthrough(),
  ],
  [
    "context_debug",
    z
      .object({
        run_id: uuidSchema,
        messages: jsonSchema,
        tool_names: z.array(z.string()),
        total_tokens: uintSchema,
        system_tokens: uintSchema,
        history_message_count: uintSchema,
        agent_id: stringSchema,
        agent_name: z.string().nullable(),
        ts: timestampSchema,
      })
      .passthrough(),
  ],
  [
    "run_warning",
    z
      .object({
        run_id: uuidSchema,
        warning: z.object({ code: stringSchema, message: stringSchema }).passthrough(),
        source_agent: stringSchema.optional(),
      })
      .passthrough(),
  ],
  [
    "run_finished",
    z
      .object({
        run_id: uuidSchema,
        ok: z.boolean(),
        prompt_tokens: uintSchema,
        completion_tokens: uintSchema,
        reasoning_tokens: uintSchema.optional(),
        cache_creation_input_tokens: uintSchema.optional(),
        cache_read_input_tokens: uintSchema.optional(),
        ts: timestampSchema,
      })
      .passthrough(),
  ],
  ["run_error", runErrorSchema],
  ["run_cancelled", z.object({ run_id: uuidSchema, ts: timestampSchema }).passthrough()],
  ["session_activity_started", sessionActivitySchema],
  ["session_activity_ended", sessionActivitySchema],
  ["stream_state", streamStateSchema],
] as const satisfies ReadonlyArray<readonly [string, ZodType]>;

const sseSchemas = new Map<string, ZodType>(sseSchemaEntries);
export const contractedSseTypes = Object.freeze(sseSchemaEntries.map(([type]) => type));

const settingsSchema = z
  .object({
    version: stringSchema,
    provider: stringSchema,
    model: stringSchema,
    posture: stringSchema,
    base_url: stringSchema,
    stream_chunk_timeout_secs: z.number().nonnegative(),
    llm_providers: z.array(stringSchema),
    agents: z.array(
      z
        .object({
          id: uuidSchema,
          name: stringSchema,
          is_default: z.boolean(),
          model: z.string().nullable(),
          needs_bootstrap: z.boolean(),
        })
        .passthrough(),
    ),
    context: z
      .object({
        strategy: stringSchema,
        max_input_tokens: uintSchema,
        compact_trigger_pct: z.number(),
        compact_retain_pct: z.number(),
        summary_model: z.string().nullable(),
        summary_provider: z.string().nullable(),
      })
      .passthrough(),
    session: z
      .object({
        max_messages: uintSchema,
        max_context_tokens: uintSchema,
        idle_timeout_secs: uintSchema,
        auto_archive: z.boolean(),
        archive_ttl_secs: uintSchema,
      })
      .passthrough(),
    tools: z
      .object({
        sandbox_root: z.string(),
        shell_policy: stringSchema,
        timeout_secs: uintSchema,
        max_output_bytes: uintSchema,
        enabled: z.array(stringSchema),
      })
      .passthrough(),
    logging: z
      .object({
        file_enabled: z.boolean(),
        file_level: stringSchema,
        rotation: stringSchema,
        log_dir: z.string().nullable(),
      })
      .passthrough(),
    llm: z
      .object({
        anthropic: z
          .object({ thinking_budget_tokens: uintSchema, prompt_cache_enabled: z.boolean() })
          .passthrough(),
        openai: z.object({ reasoning_effort: z.string().nullable() }).passthrough(),
        gemini: z
          .object({
            thinking_budget: uintSchema.nullable(),
            cache_enabled: z.boolean(),
            cache_ttl_seconds: uintSchema,
          })
          .passthrough(),
      })
      .passthrough(),
  })
  .passthrough();

const apiContracts: ReadonlyArray<{
  method: string;
  matches: (url: URL) => boolean;
  boundary: string;
  schema: ZodType;
}> = [
  {
    method: "GET",
    matches: (url) => url.pathname === "/agents",
    boundary: "GET /agents",
    schema: agentsSnapshotSchema,
  },
  {
    method: "POST",
    matches: (url) => url.pathname === "/agents",
    boundary: "POST /agents",
    schema: agentSchema,
  },
  {
    method: "GET",
    matches: (url) => /^\/agents\/[^/]+\/timeline$/.test(url.pathname),
    boundary: "GET /agents/{id}/timeline",
    schema: z
      .object({
        agent_id: uuidSchema,
        agent_name: stringSchema,
        events: z.array(
          z
            .object({
              timestamp: timestampSchema,
              event_type: stringSchema,
              session_id: uuidSchema,
              session_type: stringSchema,
              context_id: z.string(),
              run_id: uuidSchema.optional(),
              summary: z.string(),
              metadata: objectPayloadSchema.optional(),
            })
            .passthrough(),
        ),
        pagination: z
          .object({ limit: uintSchema, has_more: z.boolean(), next_before: z.string().nullable() })
          .passthrough(),
      })
      .passthrough(),
  },
  {
    method: "GET",
    matches: (url) => /^\/agents\/[^/]+\/workspace$/.test(url.pathname),
    boundary: "GET /agents/{id}/workspace",
    schema: z
      .object({ agent_id: uuidSchema, files: z.record(z.string(), z.string()) })
      .passthrough(),
  },
  {
    method: "PUT",
    matches: (url) => /^\/agents\/[^/]+\/workspace\/[^/]+$/.test(url.pathname),
    boundary: "PUT /agents/{id}/workspace/{file}",
    schema: z.object({ ok: okSchema, file: stringSchema }).passthrough(),
  },
  {
    method: "POST",
    matches: (url) => /^\/agents\/[^/]+\/workspace\/open$/.test(url.pathname),
    boundary: "POST /agents/{id}/workspace/open",
    schema: z.object({ ok: okSchema, path: stringSchema }).passthrough(),
  },
  {
    method: "POST",
    matches: (url) => /^\/agents\/[^/]+\/default$/.test(url.pathname),
    boundary: "POST /agents/{id}/default",
    schema: z.object({ ok: okSchema, default_agent: stringSchema }).passthrough(),
  },
  {
    method: "GET",
    matches: (url) => /^\/agents\/[^/]+$/.test(url.pathname),
    boundary: "GET /agents/{id}",
    schema: agentSchema,
  },
  {
    method: "PUT",
    matches: (url) => /^\/agents\/[^/]+$/.test(url.pathname),
    boundary: "PUT /agents/{id}",
    schema: agentSchema,
  },
  {
    method: "DELETE",
    matches: (url) => /^\/agents\/[^/]+$/.test(url.pathname),
    boundary: "DELETE /agents/{id}",
    schema: z.object({ ok: okSchema, deleted: uuidSchema }).passthrough(),
  },
  {
    method: "GET",
    matches: (url) => url.pathname === "/sessions",
    boundary: "GET /sessions",
    schema: sessionsSnapshotSchema,
  },
  {
    method: "POST",
    matches: (url) => url.pathname === "/sessions",
    boundary: "POST /sessions",
    schema: z.object({ session_id: uuidSchema, created: z.boolean() }).passthrough(),
  },
  {
    method: "GET",
    matches: (url) => /^\/sessions\/[^/]+\/messages$/.test(url.pathname),
    boundary: "GET /sessions/{id}/messages",
    schema: z
      .object({ messages: z.array(messageSchema), last_event_id: nullableCursorSchema })
      .passthrough(),
  },
  {
    method: "GET",
    matches: (url) => /^\/sessions\/[^/]+\/tool-calls$/.test(url.pathname),
    boundary: "GET /sessions/{id}/tool-calls",
    schema: z
      .object({
        session_id: uuidSchema,
        tool_calls: z.array(
          z
            .object({
              run_id: uuidSchema,
              seq: uintSchema,
              role: z.enum(["assistant", "tool"]),
              timestamp: timestampSchema,
              tool_name: stringSchema.optional(),
              tool_id: stringSchema.optional(),
              params: z.string().optional(),
              result: z.string().optional(),
              from_agent: stringSchema.optional(),
            })
            .passthrough(),
        ),
      })
      .passthrough(),
  },
  {
    method: "POST",
    matches: (url) => /^\/sessions\/[^/]+\/cancel-dm$/.test(url.pathname),
    boundary: "POST /sessions/{id}/cancel-dm",
    schema: z
      .object({
        ok: okSchema,
        session_id: uuidSchema,
        context_id: z.string(),
        participants: z.tuple([stringSchema, stringSchema]),
        runs_cancelled: uintSchema,
        reason: z.literal("user_cancelled"),
      })
      .passthrough(),
  },
  {
    method: "POST",
    matches: (url) => /^\/sessions\/[^/]+\/subagent\/cancel$/.test(url.pathname),
    boundary: "POST /sessions/{id}/subagent/cancel",
    schema: z.object({ session_id: uuidSchema, status: z.literal("cancelling") }).passthrough(),
  },
  {
    method: "DELETE",
    matches: (url) => /^\/sessions\/[^/]+$/.test(url.pathname),
    boundary: "DELETE /sessions/{id}",
    schema: z.object({ ok: okSchema, deleted: uuidSchema }).passthrough(),
  },
  {
    method: "GET",
    matches: (url) => /^\/session\/[^/]+$/.test(url.pathname),
    boundary: "GET /session/{id}",
    schema: sessionSchema,
  },
  {
    method: "POST",
    matches: (url) => url.pathname === "/runs",
    boundary: "POST /runs",
    schema: z
      .object({
        run_id: uuidSchema,
        session_id: uuidSchema,
        status: z.literal("queued"),
        ts: timestampSchema,
      })
      .passthrough(),
  },
  {
    method: "GET",
    matches: (url) => url.pathname === "/runs" && url.searchParams.has("agent_id"),
    boundary: "GET /runs?agent_id",
    schema: z.object({ runs: z.array(agentRunSchema) }).passthrough(),
  },
  {
    method: "GET",
    matches: (url) => url.pathname === "/runs",
    boundary: "GET /runs",
    schema: z.object({ runs: z.array(sessionRunSchema) }).passthrough(),
  },
  {
    method: "GET",
    matches: (url) => /^\/runs\/[^/]+\/reasoning$/.test(url.pathname),
    boundary: "GET /runs/{id}/reasoning",
    schema: z
      .object({
        run_id: uuidSchema,
        text: z.string(),
        last_event_id: nullableCursorSchema,
        terminal: z.boolean(),
        seal_event_id: nullableCursorSchema,
      })
      .passthrough(),
  },
  {
    method: "GET",
    matches: (url) => /^\/runs\/[^/]+\/text$/.test(url.pathname),
    boundary: "GET /runs/{id}/text",
    schema: z
      .object({ run_id: uuidSchema, text: z.string(), last_event_id: nullableCursorSchema })
      .passthrough(),
  },
  {
    method: "POST",
    matches: (url) => /^\/runs\/[^/]+\/cancel$/.test(url.pathname),
    boundary: "POST /runs/{id}/cancel",
    schema: z.object({ run_id: uuidSchema, status: z.literal("cancelled") }).passthrough(),
  },
  {
    method: "GET",
    matches: (url) => /^\/runs\/[^/]+$/.test(url.pathname),
    boundary: "GET /runs/{id}",
    schema: sessionRunSchema,
  },
  {
    method: "GET",
    matches: (url) => url.pathname === "/approvals",
    boundary: "GET /approvals",
    schema: z
      .object({
        approvals: z.array(
          z
            .object({
              approval_id: uuidSchema,
              run_id: uuidSchema,
              tool: stringSchema,
              params: jsonSchema,
              requested_at: timestampSchema,
            })
            .passthrough(),
        ),
      })
      .passthrough(),
  },
  {
    method: "POST",
    matches: (url) => /^\/approvals\/[^/]+$/.test(url.pathname),
    boundary: "POST /approvals/{id}",
    schema: z.object({ ok: okSchema }).passthrough(),
  },
  {
    method: "GET",
    matches: (url) => url.pathname === "/settings",
    boundary: "GET /settings",
    schema: settingsSchema,
  },
  {
    method: "PATCH",
    matches: (url) => url.pathname === "/settings",
    boundary: "PATCH /settings",
    schema: z
      .object({
        status: z.literal("ok"),
        restart_required: z.literal(true).optional(),
        restart_reason: stringSchema.optional(),
      })
      .passthrough(),
  },
  {
    method: "GET",
    matches: (url) => url.pathname === "/auth/keys",
    boundary: "GET /auth/keys",
    schema: z
      .object({
        keys: z.array(
          z
            .object({
              provider: stringSchema,
              configured: z.boolean(),
              key: z.string().nullable(),
              source: z.enum(["secrets", "none"]),
            })
            .passthrough(),
        ),
      })
      .passthrough(),
  },
  {
    method: "PUT",
    matches: (url) => url.pathname === "/auth/keys",
    boundary: "PUT /auth/keys",
    schema: z.object({ ok: okSchema, provider: stringSchema, key: stringSchema }).passthrough(),
  },
  {
    method: "DELETE",
    matches: (url) => /^\/auth\/keys\/[^/]+$/.test(url.pathname),
    boundary: "DELETE /auth/keys/{provider}",
    schema: z.object({ ok: okSchema, removed: z.boolean(), provider: stringSchema }).passthrough(),
  },
  {
    method: "GET",
    matches: (url) => url.pathname === "/jobs",
    boundary: "GET /jobs",
    schema: z.object({ jobs: z.array(jobSchema) }).passthrough(),
  },
  {
    method: "POST",
    matches: (url) => url.pathname === "/jobs",
    boundary: "POST /jobs",
    schema: jobSchema,
  },
  {
    method: "DELETE",
    matches: (url) => /^\/jobs\/[^/]+$/.test(url.pathname),
    boundary: "DELETE /jobs/{id}",
    schema: jobSchema,
  },
  {
    method: "GET",
    matches: (url) => url.pathname === "/audit",
    boundary: "GET /audit",
    schema: z
      .object({
        events: z.array(
          z
            .object({
              session_id: uuidSchema,
              run_id: uuidSchema.nullable(),
              tool: stringSchema,
              decision: z.enum(["allow", "deny", "error"]),
              params: jsonSchema,
              result: jsonSchema.nullable(),
              error: z.string().nullable(),
              timestamp: timestampSchema,
            })
            .passthrough(),
        ),
      })
      .passthrough(),
  },
];

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
  const schema = sseSchemas.get(type);
  if (!schema) {
    throw new ContractViolation(`SSE ${type}`, [
      { code: "custom", path: [], message: "event type is not contracted" },
    ]);
  }
  return parseAtBoundary(`SSE ${type}`, schema, input);
}

export function parseApiResponse(path: string, method: string, input: unknown): unknown {
  const url = new URL(path, "http://alms.local");
  const normalizedMethod = method.toUpperCase();
  const contract = apiContracts.find(
    (candidate) => candidate.method === normalizedMethod && candidate.matches(url),
  );
  if (!contract) {
    throw new ContractViolation(`${normalizedMethod} ${url.pathname}`, [
      { code: "custom", path: [], message: "route is not contracted" },
    ]);
  }
  return parseAtBoundary(contract.boundary, contract.schema, input);
}
