import { describe, expect, it } from "vitest";

import { ContractViolation, parseSsePayload } from "./contracts";

const runId = "22222222-2222-4222-8222-222222222222";

describe("context_debug wire contract", () => {
  it("accepts the field names emitted by the Rust gateway", () => {
    expect(
      parseSsePayload("context_debug", {
        run_id: runId,
        messages: [],
        tool_names: ["fs_read"],
        total_tokens: 120,
        system_tokens: 20,
        history_message_count: 3,
        agent_id: "atlas",
        agent_name: null,
        ts: "2026-07-12T10:00:00Z",
      }),
    ).toMatchObject({ total_tokens: 120, history_message_count: 3 });
  });

  it("rejects the nonexistent field-name variant", () => {
    expect(() =>
      parseSsePayload("context_debug", {
        run_id: runId,
        messages: [],
        tool_names: [],
        message_count: 3,
        estimated_tokens: 120,
        max_input_tokens: 1_000,
        agent_id: "atlas",
        agent_name: null,
        ts: "2026-07-12T10:00:00Z",
      }),
    ).toThrow(ContractViolation);
  });
});
