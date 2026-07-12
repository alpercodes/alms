import fixtures from "./wire-fixtures.json";
import { describe, expect, it } from "vitest";

import { parseApiResponse, parseSsePayload } from "./contracts";

describe("Rust-serialized wire fixtures", () => {
  it("accepts the exact current run-status DTO", () => {
    expect(
      parseApiResponse("/runs/11111111-1111-4111-8111-111111111111", "GET", fixtures.run_status),
    ).toEqual(fixtures.run_status);
  });

  for (const type of ["subagent_activity", "run_error", "approval_required"] as const) {
    it("accepts the exact current " + type + " SSE payload", () => {
      expect(parseSsePayload(type, fixtures.sse[type])).toEqual(fixtures.sse[type]);
    });
  }
});
