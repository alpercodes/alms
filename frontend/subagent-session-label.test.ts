import { render } from "@testing-library/preact";
import { h } from "preact";
import { beforeEach, describe, expect, it } from "vitest";

import type {
  ImageMessage as ImageMessageComponent,
  Message as MessageComponent,
  ThinkingMessage as ThinkingMessageComponent,
} from "../crates/alms-gateway/static/ui/components/chat/message.js";
import { installStateBridge, type AlmsStateBridge } from "./state/bridge";
import type { AgentEntity, SessionEntity } from "./state/core-store";

const PARENT_ID = "00000000-0000-4000-8000-000000000001";
const PEER_ID = "00000000-0000-4000-8000-000000000002";
// #1278 files a named subagent session under the invoked agent's REGISTRY id
// when it has one, so `sessionOwnerName`'s `agent_id` arm resolves for those.
// The derived ids below are the two cases it still cannot reach — a name that
// was never registered, and an ephemeral subagent — which is why the owner
// keeps arriving as `agent_name`.
const REGISTERED_REVIEWER_ID = "00000000-0000-4000-8000-000000000003";
const DERIVED_NAMED_ID = "00000000-0000-4000-8000-0000000000d1";
const DERIVED_EPHEMERAL_ID = "00000000-0000-4000-8000-0000000000d2";
const CHAT_SESSION = "00000000-0000-4000-8000-000000000010";
const NAMED_SUBAGENT_SESSION = "00000000-0000-4000-8000-000000000011";
const EPHEMERAL_SUBAGENT_SESSION = "00000000-0000-4000-8000-000000000012";
const UNKNOWN_OWNER_SESSION = "00000000-0000-4000-8000-000000000013";
const UNFETCHED_SUBAGENT_SESSION = "00000000-0000-4000-8000-000000000014";
const REGISTERED_SUBAGENT_SESSION = "00000000-0000-4000-8000-000000000015";
const RENAMED_SUBAGENT_SESSION = "00000000-0000-4000-8000-000000000016";
const TASK_ID = "00000000-0000-4000-8000-0000000000aa";

/** Mirrors `EPHEMERAL_SUBAGENT_LABEL` in `crates/alms-gateway/src/server/routes.rs`. */
const EPHEMERAL_LABEL = "(subagent)";

const PARENT: AgentEntity = {
  id: PARENT_ID,
  name: "atlas",
  description: "",
  has_telegram: false,
  debug_mode: false,
  is_default: true,
};

/**
 * Envelopes as `enrich_session_json` emits them: `agent_id` is whatever the
 * session is stored under, `agent_name` is the backend's owner enrichment.
 */
function subagentSession(
  id: string,
  agentId: string,
  contextId: string,
  agentName?: string,
): SessionEntity {
  return {
    id,
    agent_id: agentId,
    context_id: contextId,
    session_type: "subagent",
    has_active_run: false,
    ...(agentName === undefined ? {} : { agent_name: agentName }),
  };
}

/**
 * Regression pin for #1277: a subagent session's assistant bubbles were
 * labelled with the PARENT agent's name.
 *
 * The mislabel needed three things at once, all still true here:
 *   1. The subagent session is stored under a derived / random agent id, so
 *      the agents-list lookup in `sessionOwnerName` resolves nothing.
 *   2. Opening it does not switch the active agent, so `activeAgent` keeps
 *      pointing at the parent (#1212).
 *   3. The bubble label fell back to `activeAgent` whenever the owner did
 *      not resolve — turning "I don't know" into a confident wrong name.
 *
 * These cases drive the real render path (session envelope -> core store ->
 * `activeSession` -> `Message`), so they stay honest if the label derivation
 * moves. The `activeAgent` in every case is the parent, which is exactly the
 * name that must never appear on a session the parent does not own.
 */
describe("assistant bubble author label", () => {
  let bridge: AlmsStateBridge;
  let Message: typeof MessageComponent;
  let activeSessionId: { value: string | null };
  let activeAgentId: { value: string | null };

  beforeEach(async () => {
    bridge = installStateBridge();
    ({ Message } = await import("../crates/alms-gateway/static/ui/components/chat/message.js"));
    ({ activeSessionId } = await import("../crates/alms-gateway/static/ui/state/sessions.js"));
    ({ activeAgentId } = await import("../crates/alms-gateway/static/ui/state/agents.js"));

    bridge.resetScopedState();
    bridge.replaceAgents([PARENT]);
    activeAgentId.value = PARENT_ID;
  });

  function renderLabel(): string {
    const { container } = render(h(Message, { type: "agent", text: "done", sealed: true }));
    const label = container.querySelector(".msg-label");
    if (!label) throw new Error("no author label rendered");
    return label.textContent ?? "";
  }

  function labelFor(session: SessionEntity): string {
    bridge.upsertSession(session, "pinned");
    activeSessionId.value = session.id;
    return renderLabel();
  }

  it("names the subagent, not the parent, on a named subagent session", () => {
    const label = labelFor(
      subagentSession(
        NAMED_SUBAGENT_SESSION,
        DERIVED_NAMED_ID,
        `subagent_${PARENT_ID}_reviewer`,
        "reviewer",
      ),
    );

    expect(label).toBe("reviewer $");
    expect(label).not.toContain("atlas");
  });

  it("shows a non-name marker, not the parent, on an ephemeral subagent session", () => {
    const label = labelFor(
      subagentSession(
        EPHEMERAL_SUBAGENT_SESSION,
        DERIVED_EPHEMERAL_ID,
        `subagent_${PARENT_ID}_${TASK_ID}`,
        EPHEMERAL_LABEL,
      ),
    );

    expect(label).toBe(`${EPHEMERAL_LABEL} $`);
    expect(label).not.toContain("atlas");
  });

  it("renders no name at all when the owner cannot be resolved", () => {
    // Stands in for the next session type that resolves to neither an
    // `agent_name` nor a known `agent_id` — the acceptance rule is that it
    // degrades to blank rather than to whoever is selected in the sidebar.
    const label = labelFor(
      subagentSession(UNKNOWN_OWNER_SESSION, DERIVED_NAMED_ID, "subagent_malformed"),
    );

    expect(label).toBe("$");
    expect(label).not.toContain("atlas");
  });

  it("still names the active agent on its own chat session", () => {
    const label = labelFor({
      id: CHAT_SESSION,
      agent_id: PARENT_ID,
      context_id: "web-chat",
      session_type: "chat",
      has_active_run: false,
    });

    expect(label).toBe("atlas $");
  });

  it("renders no name when a session is selected but its envelope never loaded", () => {
    // The hole an envelope-gated fallback leaves open. Subagent sessions are
    // excluded from `list_sessions` outright, so the ONLY thing that puts one
    // in the store is the single-session GET in `load-session.js` — inside a
    // `try` whose `catch` is explicitly "Non-fatal — log and continue". When
    // that fetch 404s or the network blips, the session is on screen with no
    // envelope behind it, which is exactly the state that used to hand the
    // bubbles to whoever the sidebar had selected. No `upsertSession` here:
    // that IS the failure being reproduced.
    activeSessionId.value = UNFETCHED_SUBAGENT_SESSION;

    const label = renderLabel();
    expect(label).toBe("$");
    expect(label).not.toContain("atlas");
  });

  it("names the active agent when nothing is selected at all", () => {
    // The one case the old `|| activeAgent?.name` fallback was really for:
    // boot, or after `switchAgent` clears the selection. Nothing else is in
    // play, so the active agent is the correct author. This is the row that
    // keeps the tightening above from collapsing into "never fall back".
    activeSessionId.value = null;

    expect(renderLabel()).toBe("atlas $");
  });

  it("names the subagent when BOTH resolution arms answer (#1278)", () => {
    // New state as of #1278: the session is filed under the invoked agent's
    // registry id, so `agent_id` resolves — on top of the `agent_name`
    // enrichment that was previously the only answer. The two agree by
    // construction (the registry id was looked up BY that name), so the
    // label is unchanged; this pins that it stays unchanged rather than
    // starting to depend on which arm runs first.
    bridge.replaceAgents([
      PARENT,
      { ...PARENT, id: REGISTERED_REVIEWER_ID, name: "reviewer", is_default: false },
    ]);

    const label = labelFor(
      subagentSession(
        REGISTERED_SUBAGENT_SESSION,
        REGISTERED_REVIEWER_ID,
        `subagent_${PARENT_ID}_reviewer`,
        "reviewer",
      ),
    );

    expect(label).toBe("reviewer $");
    expect(label).not.toContain("atlas");
  });

  it("prefers the context's name over the registry's when the agent was renamed", () => {
    // The one way the two arms can DISAGREE: the agent has since been
    // renamed, so its registry record says "critic" while the session's
    // context still says "reviewer". `agent_name` wins, which names the
    // session for what it was when it ran — and either way the answer is
    // the subagent, never the parent, which is #1277's actual rule.
    //
    // Not reachable through the API today (Tim N2 on PR #1288): there is
    // no rename route — `UpdateAgentRequest` has no `name` field and
    // `PUT /agents/{id_or_name}` is the only update endpoint — so do not
    // go looking for one. This is future-proofing for the day a rename
    // lands, which is exactly when the divergence stops being hypothetical
    // and the wrong arm would start renaming history retroactively.
    bridge.replaceAgents([
      PARENT,
      { ...PARENT, id: REGISTERED_REVIEWER_ID, name: "critic", is_default: false },
    ]);

    const label = labelFor(
      subagentSession(
        RENAMED_SUBAGENT_SESSION,
        REGISTERED_REVIEWER_ID,
        `subagent_${PARENT_ID}_reviewer`,
        "reviewer",
      ),
    );

    expect(label).toBe("reviewer $");
    expect(label).not.toContain("atlas");
  });

  it("names the owning agent of a chat session the sidebar is not focused on", () => {
    // Guards the other direction: the fallback tightening must not blank out
    // a session whose owner IS resolvable, just because it is not the active
    // agent's.
    bridge.replaceAgents([PARENT, { ...PARENT, id: PEER_ID, name: "larry", is_default: false }]);

    const label = labelFor({
      id: CHAT_SESSION,
      agent_id: PEER_ID,
      context_id: "web-chat",
      session_type: "chat",
      has_active_run: false,
    });

    expect(label).toBe("larry $");
  });
});

/**
 * The same rule, one call site over.
 *
 * The live run indicator sits in the same `.msg-label` slot as the bubbles
 * above, in the same message list, but read `activeAgent` directly instead of
 * the session-aware computed — so it survived the first pass at #1277 and
 * still named the parent. A live run on a subagent drill-down is a designed
 * state, not a corner case: the breadcrumb renders a "Cancel subagent" button
 * gated on exactly it.
 */
describe("live run indicator author label", () => {
  let bridge: AlmsStateBridge;
  let ThinkingMessage: typeof ThinkingMessageComponent;
  let activeSessionId: { value: string | null };
  let activeAgentId: { value: string | null };

  beforeEach(async () => {
    bridge = installStateBridge();
    ({ ThinkingMessage } =
      await import("../crates/alms-gateway/static/ui/components/chat/message.js"));
    ({ activeSessionId } = await import("../crates/alms-gateway/static/ui/state/sessions.js"));
    ({ activeAgentId } = await import("../crates/alms-gateway/static/ui/state/agents.js"));

    bridge.resetScopedState();
    bridge.replaceAgents([PARENT]);
    activeAgentId.value = PARENT_ID;
  });

  function thinkingLabel(): string {
    // The plain in-flight state: not queued, not a pending send, no source —
    // i.e. the bare "Thinking" row an operator watches on any live run.
    const { container } = render(
      h(ThinkingMessage, { pending: false, queuedBehind: 0, source: null }),
    );
    const label = container.querySelector(".msg-label");
    if (!label) throw new Error("no author label rendered");
    return label.textContent ?? "";
  }

  it("names the subagent, not the parent, while a subagent run is live", () => {
    bridge.upsertSession(
      subagentSession(
        NAMED_SUBAGENT_SESSION,
        DERIVED_NAMED_ID,
        `subagent_${PARENT_ID}_reviewer`,
        "reviewer",
      ),
      "pinned",
    );
    activeSessionId.value = NAMED_SUBAGENT_SESSION;

    const label = thinkingLabel();
    expect(label).toBe("reviewer $");
    expect(label).not.toContain("atlas");
  });

  it("falls back to the neutral marker, never the parent, when the owner is unknown", () => {
    // Covers both unresolvable shapes at once: an envelope that resolves to no
    // owner. `Agent` is deliberate rather than a bare `$` — this row is
    // transient run status, not something an agent said, and uppercase is
    // rejected by `validate_agent_name`, so it can never collide with a real
    // agent's name. What matters for #1277 is only that it is not the parent.
    bridge.upsertSession(
      subagentSession(UNKNOWN_OWNER_SESSION, DERIVED_NAMED_ID, "subagent_malformed"),
      "pinned",
    );
    activeSessionId.value = UNKNOWN_OWNER_SESSION;

    const label = thinkingLabel();
    expect(label).toBe("Agent $");
    expect(label).not.toContain("atlas");
  });

  it("renders the neutral marker when the subagent envelope never loaded", () => {
    // The Critical-2 path for the indicator: session selected, envelope fetch
    // failed, nothing in the store to identify the owner.
    activeSessionId.value = UNFETCHED_SUBAGENT_SESSION;

    const label = thinkingLabel();
    expect(label).toBe("Agent $");
    expect(label).not.toContain("atlas");
  });

  it("still names the active agent on its own chat session", () => {
    // The common case must keep its name: the indicator is what the operator
    // watches on every ordinary run, so blanking it would be a real regression.
    bridge.upsertSession(
      {
        id: CHAT_SESSION,
        agent_id: PARENT_ID,
        context_id: "web-chat",
        session_type: "chat",
        has_active_run: true,
      },
      "agent",
    );
    activeSessionId.value = CHAT_SESSION;

    expect(thinkingLabel()).toBe("atlas $");
  });
});

/**
 * The same rule again, on the last row that carried its own copy of it.
 *
 * The image row lived inline in `app.js`'s message-list `.map()` with a
 * hand-copied label rule. No test in `frontend/` imports `app.js`, so that
 * copy could be reverted to `activeAgent` — reinstating #1277 exactly — with
 * all 71 rows of this suite still passing. It now renders through the shared
 * `ImageMessage`, which takes its label from the same `authorLabel` as the
 * bubbles above, and these rows are the harness that copy never had.
 */
describe("image row author label", () => {
  let bridge: AlmsStateBridge;
  let ImageMessage: typeof ImageMessageComponent;
  let activeSessionId: { value: string | null };
  let activeAgentId: { value: string | null };

  beforeEach(async () => {
    bridge = installStateBridge();
    ({ ImageMessage } =
      await import("../crates/alms-gateway/static/ui/components/chat/message.js"));
    ({ activeSessionId } = await import("../crates/alms-gateway/static/ui/state/sessions.js"));
    ({ activeAgentId } = await import("../crates/alms-gateway/static/ui/state/agents.js"));

    bridge.resetScopedState();
    bridge.replaceAgents([PARENT]);
    activeAgentId.value = PARENT_ID;
  });

  function renderImage(props: Record<string, unknown>): HTMLElement {
    const { container } = render(
      h(ImageMessage, { url: "data:image/png;base64,AAAA", alt: "chart", ...props }),
    );
    const row = container.querySelector(".msg");
    if (!row) throw new Error("no image row rendered");
    return row as HTMLElement;
  }

  function imageLabel(props: Record<string, unknown> = {}): string {
    return renderImage(props).querySelector(".msg-label")?.textContent ?? "";
  }

  it("names the subagent, not the parent, on a subagent session", () => {
    bridge.upsertSession(
      subagentSession(
        NAMED_SUBAGENT_SESSION,
        DERIVED_NAMED_ID,
        `subagent_${PARENT_ID}_reviewer`,
        "reviewer",
      ),
      "pinned",
    );
    activeSessionId.value = NAMED_SUBAGENT_SESSION;

    const label = imageLabel({ role: "assistant" });
    expect(label).toBe("reviewer $");
    expect(label).not.toContain("atlas");
  });

  it("renders no name at all when the owner cannot be resolved", () => {
    bridge.upsertSession(
      subagentSession(UNKNOWN_OWNER_SESSION, DERIVED_NAMED_ID, "subagent_malformed"),
      "pinned",
    );
    activeSessionId.value = UNKNOWN_OWNER_SESSION;

    const label = imageLabel({ role: "assistant" });
    expect(label).toBe("$");
    expect(label).not.toContain("atlas");
  });

  it("still names the active agent on its own chat session", () => {
    bridge.upsertSession(
      {
        id: CHAT_SESSION,
        agent_id: PARENT_ID,
        context_id: "web-chat",
        session_type: "chat",
        has_active_run: false,
      },
      "agent",
    );
    activeSessionId.value = CHAT_SESSION;

    expect(imageLabel({ role: "assistant" })).toBe("atlas $");
  });

  it("keeps a user-uploaded image on the user side with no author name", () => {
    // The label rule has two inputs, and the owner tightening must not reach
    // the one that decides "this is the operator's own upload" (#546).
    activeSessionId.value = CHAT_SESSION;

    const row = renderImage({ role: "user" });
    expect(row.className).toContain("user");
    expect(row.querySelector(".msg-label")?.textContent).toBe(">");
  });

  it("names the sender of a DM image and renders it as an agent row", () => {
    // A DM image arrives with role `user` but a `fromAgent` — it is a peer
    // agent speaking, so it belongs on the agent side under the peer's name,
    // never the session owner's and never the sidebar's selection (#546).
    bridge.upsertSession(
      subagentSession(
        NAMED_SUBAGENT_SESSION,
        DERIVED_NAMED_ID,
        `subagent_${PARENT_ID}_reviewer`,
        "reviewer",
      ),
      "pinned",
    );
    activeSessionId.value = NAMED_SUBAGENT_SESSION;

    const row = renderImage({ role: "user", fromAgent: "larry" });
    expect(row.className).toContain("agent");
    expect(row.querySelector(".msg-label")?.textContent).toBe("larry $");
  });
});
