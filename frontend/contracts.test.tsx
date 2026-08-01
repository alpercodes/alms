import { screen } from "@testing-library/preact";
import { describe, expect, it, vi } from "vitest";

import { installContractBridge } from "./bridge";
import { ContractViolation, jobSchema, parseSsePayload } from "./contracts";

const sessionId = "11111111-1111-4111-8111-111111111111";
const runId = "22222222-2222-4222-8222-222222222222";
const agentId = "33333333-3333-4333-8333-333333333333";
const epoch = "44444444-4444-4444-8444-444444444444";

describe("typed compatibility boundary", () => {
  it("accepts the authoritative stream-state envelope", () => {
    expect(
      parseSsePayload("stream_state", {
        stream_epoch: epoch,
        retained_from: 10,
        newest: 20,
        replay_gap: false,
        epoch_mismatch: false,
        requires_reconciliation: false,
      }),
    ).toMatchObject({ newest: 20 });
  });

  it("accepts authoritative completed and failed job states", () => {
    const base = {
      id: runId,
      prompt: "check durable state",
      schedule: { type: "once" as const, run_at: "2026-07-12T10:00:00Z" },
      next_run_at: null,
      last_run_at: "2026-07-12T10:00:05Z",
    };

    expect(
      jobSchema.parse({
        ...base,
        status: "completed",
        terminal_reason: "deadline_reached",
        retry_count: 0,
      }).status,
    ).toBe("completed");
    expect(
      jobSchema.parse({
        ...base,
        status: "failed",
        terminal_reason: "retry_exhausted",
        retry_count: 3,
        last_error: "database unavailable",
      }).status,
    ).toBe("failed");
  });
  it("rejects malformed activity without mutating legacy state", () => {
    expect(() =>
      parseSsePayload("session_activity_started", {
        session_id: sessionId,
        run_id: runId,
        agent_id: agentId,
        has_active_run: "yes",
        ts: "2026-07-12T10:00:00Z",
      }),
    ).toThrow(ContractViolation);
  });

  it("surfaces contract failures visibly through the bridge", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const bridge = installContractBridge();

    expect(() =>
      bridge.parseApiResponse("/sessions", "GET", {
        sessions: [{ id: sessionId, session_type: "chat", has_active_run: "yes" }],
      }),
    ).toThrow(ContractViolation);

    expect(screen.getByRole("alert")).toHaveTextContent("Live data rejected");
    expect(screen.getByRole("alert")).toHaveTextContent("has_active_run");
  });

  it("rejects unknown SSE types instead of bypassing the contract boundary", () => {
    expect(() => parseSsePayload("future_event", { value: 1 })).toThrow(ContractViolation);
  });
});
