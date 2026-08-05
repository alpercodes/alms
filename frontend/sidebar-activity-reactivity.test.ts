import { render, waitFor } from "@testing-library/preact";
import { h } from "preact";
import { beforeEach, describe, expect, it } from "vitest";

import { installStateBridge, type AlmsStateBridge } from "./state/bridge";
import type { AgentEntity, SessionEntity } from "./state/core-store";

const AGENT_ID = "00000000-0000-4000-8000-000000000001";
const SESSION_SELECTED = "00000000-0000-4000-8000-000000000010";
const SESSION_OTHER = "00000000-0000-4000-8000-000000000011";
const RUN_ID = "00000000-0000-4000-8000-000000000020";
const TS = "2026-01-01T00:00:00Z";

const AGENT: AgentEntity = {
  id: AGENT_ID,
  name: "atlas",
  description: "",
  has_telegram: false,
  debug_mode: false,
  is_default: true,
};

function chatSession(id: string): SessionEntity {
  return {
    id,
    agent_id: AGENT_ID,
    context_id: `web-chat-${id}`,
    session_type: "chat",
    has_active_run: false,
  };
}

/**
 * Regression pin for the sidebar active-run dot, which has churned across
 * #1211 -> #1216 -> #1220 -> #1225 -> #1226 -> #1228 with nothing covering it.
 * #1239 came close to pinning it permanently *on* (an unreconciled `Queued`
 * row hydrated into the live registry), but that was caught in review and
 * fixed before merge — a near-miss an expensive review pass caught and a
 * cheap test would have caught earlier.
 *
 * The symptom that keeps coming back: a run starting on session B lights no
 * dot unless B happens to be the row you have selected. This drives the real
 * production path: SSE activity event -> core store -> `backgroundRuns`
 * computed -> `SessionList` row class. It is also what verifies the
 * @preact/signals 1 -> 2 jump in #1227, which defers DOM updates by an
 * animation frame and went in unreviewed.
 *
 * These cases pin behavior, not the code shape. @preact/signals tracks
 * dependencies for the whole render pass regardless of read depth, so
 * `session-list.js` is free to be refactored as long as this still passes —
 * see docs/frontend.md for what the old "must read in the component body"
 * rule got wrong and the narrower hazard that survives it.
 *
 * The assertion that matters is the NON-selected row: `activeSessionId` is
 * never touched after the initial render, so the only thing that can light
 * that row is a live per-row subscription to `bgRuns`.
 */
describe("sidebar active-run dot reactivity", () => {
  let bridge: AlmsStateBridge;
  let SessionList: () => ReturnType<typeof h>;
  let activeSessionId: { value: string | null };
  let expandedAgentId: { value: string | null };
  let activeAgentId: { value: string | null };

  beforeEach(async () => {
    bridge = installStateBridge();
    ({ SessionList } =
      await import("../crates/alms-gateway/static/ui/components/sidebar/session-list.js"));
    ({ activeSessionId, expandedAgentId } =
      await import("../crates/alms-gateway/static/ui/state/sessions.js"));
    ({ activeAgentId } = await import("../crates/alms-gateway/static/ui/state/agents.js"));

    // `installStateBridge()` is a no-reset singleton and `replaceSessionScopes`
    // never touches `state.activity` — the sole input to the `backgroundRuns`
    // computed. Without this, activity leaks between cases and each test after
    // the first starts with rows already lit, making its opening assertions
    // vacuous and coupling the suite to execution order. `scoped/reset` clears
    // `activity.bySessionId` exactly.
    bridge.resetScopedState();
    bridge.replaceAgents([AGENT]);
    bridge.replaceSessionScopes([chatSession(SESSION_SELECTED), chatSession(SESSION_OTHER)], []);
    activeAgentId.value = AGENT_ID;
    expandedAgentId.value = AGENT_ID;
    activeSessionId.value = SESSION_SELECTED;
  });

  function renderSidebar() {
    const { container } = render(h(SessionList, null));
    const row = (id: string): Element => {
      const el = container.querySelector(`[title^="ID: ${id}"]`);
      if (!el) throw new Error(`no sidebar row for session ${id}`);
      return el;
    };
    return { container, row };
  }

  function activity(sessionId: string, hasActiveRun: boolean) {
    bridge.applyActivityEvent(
      hasActiveRun ? "session_activity_started" : "session_activity_ended",
      {
        session_id: sessionId,
        run_id: RUN_ID,
        agent_id: AGENT_ID,
        has_active_run: hasActiveRun,
        ts: TS,
      },
    );
  }

  it("lights a NON-selected row when a run starts on it", async () => {
    const { row } = renderSidebar();

    expect(row(SESSION_SELECTED).className).toContain("active");
    expect(row(SESSION_OTHER).className).not.toContain("has-run");

    activity(SESSION_OTHER, true);

    // `activeSessionId` is deliberately not touched — only `bgRuns` moved.
    await waitFor(() => {
      expect(row(SESSION_OTHER).className).toContain("has-run");
    });
    expect(row(SESSION_SELECTED).className).not.toContain("has-run");
  });

  it("clears the dot when the run ends", async () => {
    const { row } = renderSidebar();

    activity(SESSION_OTHER, true);
    await waitFor(() => {
      expect(row(SESSION_OTHER).className).toContain("has-run");
    });

    activity(SESSION_OTHER, false);
    await waitFor(() => {
      expect(row(SESSION_OTHER).className).not.toContain("has-run");
    });
  });

  it("lights the selected row too, without disturbing its selection", async () => {
    const { row } = renderSidebar();

    activity(SESSION_SELECTED, true);

    await waitFor(() => {
      expect(row(SESSION_SELECTED).className).toContain("has-run");
    });
    expect(row(SESSION_SELECTED).className).toContain("active");
    expect(row(SESSION_OTHER).className).not.toContain("has-run");
  });

  it("tracks concurrent runs on both rows independently", async () => {
    const { row } = renderSidebar();

    activity(SESSION_SELECTED, true);
    activity(SESSION_OTHER, true);

    await waitFor(() => {
      expect(row(SESSION_SELECTED).className).toContain("has-run");
      expect(row(SESSION_OTHER).className).toContain("has-run");
    });

    activity(SESSION_SELECTED, false);

    await waitFor(() => {
      expect(row(SESSION_SELECTED).className).not.toContain("has-run");
    });
    expect(row(SESSION_OTHER).className).toContain("has-run");
  });
});
