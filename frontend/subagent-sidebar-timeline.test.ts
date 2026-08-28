import { render } from "@testing-library/preact";
import { h } from "preact";
import { beforeEach, describe, expect, it } from "vitest";

import { installStateBridge, type AlmsStateBridge } from "./state/bridge";
import type { AgentEntity, SessionEntity } from "./state/core-store";

const ATLAS_ID = "00000000-0000-4000-8000-000000000001";
const REVIEWER_ID = "00000000-0000-4000-8000-000000000002";
const LARRY_ID = "00000000-0000-4000-8000-000000000003";
const REVIEWER_CHAT = "00000000-0000-4000-8000-000000000010";
const SUB_FROM_ATLAS = "00000000-0000-4000-8000-000000000011";
const SUB_FROM_LARRY = "00000000-0000-4000-8000-000000000012";
const UNREGISTERED_OWNER = "00000000-0000-4000-8000-0000000000d1";
const SUB_ORPHANED = "00000000-0000-4000-8000-000000000013";

function agent(id: string, name: string): AgentEntity {
  return {
    id,
    name,
    description: "",
    has_telegram: false,
    debug_mode: false,
    is_default: id === ATLAS_ID,
  };
}

const ATLAS = agent(ATLAS_ID, "atlas");
const REVIEWER = agent(REVIEWER_ID, "reviewer");
const LARRY = agent(LARRY_ID, "larry");

function chatSession(id: string, agentId: string): SessionEntity {
  return {
    id,
    agent_id: agentId,
    context_id: `web-chat-${id}`,
    session_type: "chat",
    has_active_run: false,
  };
}

/**
 * A named subagent session envelope as `enrich_session_json` emits it
 * post-#1278: `agent_id` is the INVOKED agent's registry id (so the row
 * groups under that agent), `agent_name` is the invoked agent's name, and
 * `parent_agent_id` is whoever called `invoke_agent`.
 */
function subagentSession(
  id: string,
  invokedAgentId: string,
  parentAgentId: string,
  name: string,
): SessionEntity {
  return {
    id,
    agent_id: invokedAgentId,
    context_id: `subagent_${parentAgentId}_${name}`,
    session_type: "subagent",
    agent_name: name,
    parent_agent_id: parentAgentId,
    has_active_run: false,
  };
}

/**
 * Regression pin for #1278.
 *
 * Alper's complaint was that an invoked agent's work was not in its own
 * timeline: a named subagent session was filed under
 * `AgentId::deterministic(parent, name)`, an id that matches no registered
 * agent, and `list_sessions` excluded subagent rows outright — so there
 * was no row to group and nowhere for it to go.
 *
 * Both halves had to move together, which is why this drives the real
 * render path (session envelopes -> core store -> `SessionList`) rather
 * than the pure grouping helpers: the backend can file the session
 * correctly and the sidebar can still drop it, and vice versa.
 */
describe("named subagent sessions in the invoked agent's sidebar group", () => {
  let bridge: AlmsStateBridge;
  let SessionList: () => ReturnType<typeof h>;
  let activeAgentId: { value: string | null };
  let expandedAgentId: { value: string | null };

  beforeEach(async () => {
    bridge = installStateBridge();
    ({ SessionList } =
      await import("../crates/alms-gateway/static/ui/components/sidebar/session-list.js"));
    ({ expandedAgentId } = await import("../crates/alms-gateway/static/ui/state/sessions.js"));
    ({ activeAgentId } = await import("../crates/alms-gateway/static/ui/state/agents.js"));

    bridge.resetScopedState();
    bridge.replaceAgents([ATLAS, REVIEWER, LARRY]);
    // Atlas is the selected agent throughout: the rows under test belong
    // to reviewer, and #1278 is worthless if seeing them requires
    // switching agents first.
    activeAgentId.value = ATLAS_ID;
    expandedAgentId.value = ATLAS_ID;
  });

  /** The accordion body for one agent, plus its header count badge. */
  function renderGroups() {
    const { container } = render(h(SessionList, null));
    const group = (agentId: string): Element => {
      const body = container.querySelector(`[aria-labelledby="agent-group-header-${agentId}"]`);
      if (!body) throw new Error(`no accordion body for agent ${agentId}`);
      return body;
    };
    const rowTitles = (agentId: string): string[] =>
      Array.from(group(agentId).querySelectorAll(".session-item")).map(
        (el) => el.getAttribute("title") ?? "",
      );
    const count = (agentId: string): string => {
      const header = container.querySelector(`#agent-group-header-${agentId}`);
      const badge = header?.querySelector(".agent-group-count");
      return badge?.textContent?.trim() ?? "";
    };
    return { container, group, rowTitles, count };
  }

  it("renders the subagent row under the agent that did the work", () => {
    bridge.replaceSessionScopes(
      [],
      [subagentSession(SUB_FROM_ATLAS, REVIEWER_ID, ATLAS_ID, "reviewer")],
    );

    const { rowTitles } = renderGroups();

    expect(rowTitles(REVIEWER_ID).join("\n")).toContain(`ID: ${SUB_FROM_ATLAS}`);
    // Not under the invoking parent — that was the pre-#1278 mental model
    // and it is what put the work in the wrong place.
    expect(rowTitles(ATLAS_ID).join("\n")).not.toContain(`ID: ${SUB_FROM_ATLAS}`);
    expect(rowTitles(LARRY_ID)).toEqual([]);
  });

  it("attributes the row to the INVOKING agent, not the owning one", () => {
    bridge.replaceSessionScopes(
      [],
      [subagentSession(SUB_FROM_ATLAS, REVIEWER_ID, ATLAS_ID, "reviewer")],
    );

    const { group } = renderGroups();
    const badge = group(REVIEWER_ID).querySelector(".session-agent-attribution");

    // The group header already says "reviewer"; repeating it would say
    // nothing. Who asked for the work is the informative part.
    expect(badge?.textContent).toBe("atlas");
    expect(badge?.getAttribute("title")).toBe("Invoked by atlas");
  });

  it("labels the row 'subagent', not with the raw context id", () => {
    // `subagent_{uuid}_reviewer` is 50-odd characters of uuid in a sidebar
    // row. The constant label plus the parent badge is the same split
    // notification rows use.
    bridge.replaceSessionScopes(
      [],
      [subagentSession(SUB_FROM_ATLAS, REVIEWER_ID, ATLAS_ID, "reviewer")],
    );

    const { group } = renderGroups();
    const label = group(REVIEWER_ID).querySelector(".session-label");

    expect(label?.textContent).toBe("subagent");
  });

  it("falls back to the context id when the backend could not name the owner", () => {
    // An unenriched row means the owner was unreadable. "subagent" alone
    // would then be an unclickable mystery — every such row would render
    // identically — so the context id is the more useful degradation.
    const context = `subagent_${ATLAS_ID}_reviewer`;
    bridge.replaceSessionScopes(
      [],
      [
        {
          id: SUB_FROM_ATLAS,
          agent_id: REVIEWER_ID,
          context_id: context,
          session_type: "subagent",
          has_active_run: false,
        },
      ],
    );

    const { group } = renderGroups();

    expect(group(REVIEWER_ID).querySelector(".session-label")?.textContent).toBe(context);
  });

  it("keeps two parents' rows apart inside the one agent's group", () => {
    bridge.replaceSessionScopes(
      [],
      [
        subagentSession(SUB_FROM_ATLAS, REVIEWER_ID, ATLAS_ID, "reviewer"),
        subagentSession(SUB_FROM_LARRY, REVIEWER_ID, LARRY_ID, "reviewer"),
      ],
    );

    const { group, rowTitles } = renderGroups();

    const titles = rowTitles(REVIEWER_ID).join("\n");
    expect(titles).toContain(`ID: ${SUB_FROM_ATLAS}`);
    expect(titles).toContain(`ID: ${SUB_FROM_LARRY}`);
    // Both rows carry the same label, so the badges are the only thing
    // distinguishing them — they must not both read the same name.
    const badges = Array.from(
      group(REVIEWER_ID).querySelectorAll(".session-agent-attribution"),
    ).map((el) => el.textContent);
    expect(badges).toEqual(["atlas", "larry"]);
  });

  it("lists the agent's own chats above the errands it ran for others", () => {
    activeAgentId.value = REVIEWER_ID;
    expandedAgentId.value = REVIEWER_ID;
    bridge.replaceSessionScopes(
      [chatSession(REVIEWER_CHAT, REVIEWER_ID)],
      [subagentSession(SUB_FROM_ATLAS, REVIEWER_ID, ATLAS_ID, "reviewer")],
    );

    const { rowTitles, count } = renderGroups();

    expect(rowTitles(REVIEWER_ID).map((t) => t.split("\n")[0])).toEqual([
      `ID: ${REVIEWER_CHAT}`,
      `ID: ${SUB_FROM_ATLAS}`,
    ]);
    // The header badge counts what the body renders.
    expect(count(REVIEWER_ID)).toBe("2");
  });

  it("renders a subagent row exactly once when its envelope is also pinned", () => {
    // `utils/load-session.js` Step 0 pins the active internal session's
    // envelope into the per-agent scope so `activeSession` can resolve it
    // (#1065). That copy and the cross-agent copy are the same row; the
    // sidebar must not draw both.
    const row = subagentSession(SUB_FROM_ATLAS, REVIEWER_ID, ATLAS_ID, "reviewer");
    bridge.replaceSessionScopes([], [row]);
    bridge.upsertSession(row, "pinned");

    const { rowTitles } = renderGroups();

    const matches = rowTitles(REVIEWER_ID).filter((t) => t.startsWith(`ID: ${SUB_FROM_ATLAS}`));
    expect(matches).toHaveLength(1);
  });

  it("renders nothing for a subagent whose owning agent is not registered", () => {
    // A name invoked before its agent existed keeps the pre-#1278 derived
    // id, and a deleted agent leaves its rows behind. Neither resolves to
    // a group, and inventing one would be worse than showing nothing:
    // there is no timeline to put the row in.
    bridge.replaceSessionScopes(
      [],
      [subagentSession(SUB_ORPHANED, UNREGISTERED_OWNER, ATLAS_ID, "ghost")],
    );

    const { container, rowTitles } = renderGroups();

    for (const agentId of [ATLAS_ID, REVIEWER_ID, LARRY_ID]) {
      expect(rowTitles(agentId).join("\n")).not.toContain(`ID: ${SUB_ORPHANED}`);
    }
    expect(container.textContent).not.toContain("ghost");
  });

  it("does not render subagent rows as a cross-agent section", () => {
    // Unlike DMs / notifications / jobs, these belong inside an agent
    // group. A flat section would re-create the "which agent is this?"
    // ambiguity #1278 set out to remove.
    bridge.replaceSessionScopes(
      [],
      [subagentSession(SUB_FROM_ATLAS, REVIEWER_ID, ATLAS_ID, "reviewer")],
    );

    const { container } = renderGroups();
    const dividers = Array.from(container.querySelectorAll(".session-section-divider-label")).map(
      (el) => el.textContent?.trim(),
    );

    expect(dividers).not.toContain("Subagents");
  });
});
