import { cleanup, fireEvent, render, screen } from "@testing-library/preact";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SummaryFailureBanner } from "../crates/alms-gateway/static/ui/components/chat/summary-failure-banner.js";
import {
  applySummaryStatus,
  clearSummaryFailure,
} from "../crates/alms-gateway/static/ui/state/summary-status.js";
import { ContractViolation, parseSsePayload } from "./contracts";

const sessionId = "22222222-2222-4222-8222-222222222222";
const otherSessionId = "33333333-3333-4333-8333-333333333333";
const message =
  "Conversation summaries failed: no API key is set for OpenRouter. To fix this, change the " +
  "summary model in Settings, or add an OpenRouter key in Settings (API Keys).";

/** What the gateway sends, through the same validation the live stream uses. */
function validated(input: unknown) {
  return parseSsePayload("summary_status", input) as Parameters<typeof applySummaryStatus>[0];
}

const failing = {
  session_id: sessionId,
  failure: { code: "MISSING_KEY", provider: "openrouter", model: "m", message },
};

describe("summary_status contract", () => {
  it("accepts a failure and the clear", () => {
    expect(parseSsePayload("summary_status", failing)).toEqual(failing);
    const clear = { session_id: sessionId, failure: null };
    expect(parseSsePayload("summary_status", clear)).toEqual(clear);
  });

  it("accepts an empty model, which is how no configured model looks", () => {
    const noModel = { ...failing, failure: { ...failing.failure, model: "" } };
    expect(parseSsePayload("summary_status", noModel)).toEqual(noModel);
  });

  it.each([
    ["no session", { failure: null }],
    ["no failure field", { session_id: sessionId }],
    ["unknown code", { ...failing, failure: { ...failing.failure, code: "NOPE" } }],
    ["empty message", { ...failing, failure: { ...failing.failure, message: "" } }],
  ])("rejects %s", (_name, input) => {
    expect(() => parseSsePayload("summary_status", input)).toThrow(ContractViolation);
  });
});

describe("SummaryFailureBanner", () => {
  beforeEach(() => {
    clearSummaryFailure();
  });
  afterEach(() => {
    cleanup();
  });

  it("renders nothing when there is no problem", () => {
    const { container } = render(
      <SummaryFailureBanner sessionId={sessionId} onOpenSettings={() => {}} />,
    );
    expect(container.innerHTML).toBe("");
  });

  it("shows the gateway's wording and opens Settings", () => {
    applySummaryStatus(validated(failing));
    const onOpenSettings = vi.fn();
    render(<SummaryFailureBanner sessionId={sessionId} onOpenSettings={onOpenSettings} />);

    expect(screen.getByRole("status").textContent).toContain(message);
    fireEvent.click(screen.getByRole("button", { name: "Open Settings" }));
    expect(onOpenSettings).toHaveBeenCalledOnce();
  });

  it("does not show another session's problem", () => {
    applySummaryStatus(validated(failing));
    const { container } = render(
      <SummaryFailureBanner sessionId={otherSessionId} onOpenSettings={() => {}} />,
    );
    expect(container.innerHTML).toBe("");
  });

  it("goes away when the gateway reports it fixed", () => {
    applySummaryStatus(validated(failing));
    const { rerender, container } = render(
      <SummaryFailureBanner sessionId={sessionId} onOpenSettings={() => {}} />,
    );
    expect(screen.queryByRole("status")).not.toBeNull();

    applySummaryStatus(validated({ session_id: sessionId, failure: null }));
    rerender(<SummaryFailureBanner sessionId={sessionId} onOpenSettings={() => {}} />);
    expect(container.innerHTML).toBe("");
  });

  it("can be dismissed", () => {
    // A signature no other test in this file dismisses: dismissals are module state.
    applySummaryStatus(
      validated({ ...failing, failure: { ...failing.failure, model: "dismissed-model" } }),
    );
    const { rerender, container } = render(
      <SummaryFailureBanner sessionId={sessionId} onOpenSettings={() => {}} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Dismiss summary warning" }));
    rerender(<SummaryFailureBanner sessionId={sessionId} onOpenSettings={() => {}} />);
    expect(container.innerHTML).toBe("");
  });
});
