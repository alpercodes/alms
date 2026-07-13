import { screen } from "@testing-library/preact";
import { describe, expect, it, vi } from "vitest";

import { apiFetch } from "../crates/alms-gateway/static/ui/api/client.js";
import { installContractBridge } from "./bridge";
import {
  ContractViolation,
  contractedSseTypes,
  parseApiResponse,
  parseSsePayload,
} from "./contracts";

const sessionId = "11111111-1111-4111-8111-111111111111";
const runId = "22222222-2222-4222-8222-222222222222";
const agentId = "33333333-3333-4333-8333-333333333333";

const expectedSseTypes = [
  "run_created",
  "run_started",
  "run_queue_position",
  "status",
  "token_delta",
  "reasoning_delta",
  "stream_reset",
  "tool_start",
  "tool_end",
  "approval_required",
  "subagent_activity",
  "subagent_started",
  "subagent_completed",
  "job_completed",
  "dm_message",
  "dm_conversation_ended",
  "dm_activity_started",
  "dm_activity_status",
  "dm_activity_ended",
  "approval_resolved",
  "context_debug",
  "run_warning",
  "run_finished",
  "run_error",
  "run_cancelled",
  "session_activity_started",
  "session_activity_ended",
  "stream_state",
] as const;

describe("consumed wire contract coverage", () => {
  it("keeps every registered SSE event on an explicit schema", () => {
    expect(contractedSseTypes).toEqual(expectedSseTypes);
    expect(contractedSseTypes).toHaveLength(28);
  });

  it("rejects malformed state-feeding SSE fields", () => {
    expect(() => parseSsePayload("token_delta", { run_id: runId })).toThrow(ContractViolation);
    expect(() => parseSsePayload("stream_reset", {})).toThrow(ContractViolation);
    expect(() =>
      parseSsePayload("session_activity_ended", {
        session_id: sessionId,
        run_id: runId,
        agent_id: agentId,
        has_active_run: "yes",
        ts: "2026-07-12T10:00:00Z",
      }),
    ).toThrow(ContractViolation);
  });

  it("matches query and dynamic REST routes to their explicit schemas", () => {
    expect(() =>
      parseApiResponse(`/runs?agent_id=${agentId}`, "GET", {
        runs: [{ run_id: runId, status: "running" }],
      }),
    ).toThrow(ContractViolation);

    expect(() =>
      parseApiResponse(`/runs/${runId}/reasoning`, "GET", {
        run_id: runId,
        text: "thinking",
        last_event_id: "7",
        terminal: false,
        seal_event_id: null,
      }),
    ).toThrow(ContractViolation);

    expect(() =>
      parseApiResponse(`/agents/${agentId}/workspace`, "GET", {
        agent_id: agentId,
        files: ["not", "a", "record"],
      }),
    ).toThrow(ContractViolation);
  });

  it("rejects malformed list envelopes and message discriminators", () => {
    expect(() => parseApiResponse("/settings", "GET", { agents: "bad" })).toThrow(
      ContractViolation,
    );
    expect(() => parseApiResponse("/jobs", "GET", { jobs: {} })).toThrow(ContractViolation);
    expect(() =>
      parseApiResponse(`/sessions/${sessionId}/messages`, "GET", {
        messages: [
          {
            role: "assistant",
            type: "text",
            content: 7,
            timestamp: "2026-07-12T10:00:00Z",
          },
        ],
        last_event_id: null,
      }),
    ).toThrow(ContractViolation);
  });
});

describe("guarded raw JSON wrappers", () => {
  it("prevents malformed API data from reaching caller state", async () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    installContractBridge();
    vi.stubGlobal(
      "fetch",
      vi.fn(
        () =>
          ({
            ok: true,
            status: 200,
            text: () => Promise.resolve(JSON.stringify({ agents: "bad" })),
            json: () => Promise.resolve({ agents: "bad" }),
          }) as unknown as Promise<Response>,
      ),
    );
    const state = { agents: ["unchanged"] };

    await expect(
      apiFetch("/settings").then((response) => {
        state.agents = (response as { agents: string[] }).agents;
      }),
    ).rejects.toThrow(ContractViolation);

    expect(state.agents).toEqual(["unchanged"]);
    expect(screen.getByRole("alert")).toHaveTextContent("Live data rejected");
  });

  it("surfaces syntactically invalid API and SSE JSON before handlers run", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const bridge = installContractBridge();
    let handlerCalls = 0;

    expect(() => bridge.parseApiJsonResponse("/sessions", "GET", "{bad json")).toThrow(
      ContractViolation,
    );
    expect(() => {
      const payload = bridge.parseSseJsonPayload("token_delta", "{bad json");
      handlerCalls += 1;
      return payload;
    }).toThrow(ContractViolation);

    expect(handlerCalls).toBe(0);
    expect(screen.getByRole("alert")).toHaveTextContent("Live data rejected");
  });

  it("keeps 204 responses outside object validation", async () => {
    installContractBridge();
    vi.stubGlobal(
      "fetch",
      vi.fn(() => Promise.resolve({ ok: true, status: 204 } as Response)),
    );

    await expect(apiFetch(`/jobs/${runId}`, { method: "DELETE" })).resolves.toBeNull();
  });
});
