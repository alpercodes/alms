import { expect, test, type Page } from "@playwright/test";

import { settingsFixture } from "./fixtures";

const agentA = "11111111-1111-4111-8111-111111111111";
const agentB = "11111111-1111-4111-8111-111111111112";
const sessionA = "22222222-2222-4222-8222-222222222221";
const sessionB = "22222222-2222-4222-8222-222222222222";
const streamEpoch = "33333333-3333-4333-8333-333333333333";

function session(id: string, agentId: string, contextId: string) {
  return {
    id,
    agent_id: agentId,
    context_id: contextId,
    session_type: "chat",
    has_active_run: false,
    created_at: "2026-07-13T10:00:00Z",
    updated_at: "2026-07-13T10:00:00Z",
  };
}

function textMessage(content: string) {
  return {
    role: "user",
    type: "text",
    content,
    timestamp: "2026-07-13T10:00:00Z",
  };
}

function settings() {
  const value = settingsFixture(agentA);
  value.agents = [
    { ...value.agents[0], id: agentA, name: "atlas", is_default: true },
    { ...value.agents[0], id: agentB, name: "heph", is_default: false },
  ];
  return value;
}

interface RouteOptions {
  readonly messages: (sessionId: string) => readonly ReturnType<typeof textMessage>[];
  readonly sessionEvents?: (sessionId: string, requestIndex: number) => string;
  readonly createRun?: () => Promise<void>;
}

async function installRoutes(page: Page, options: RouteOptions) {
  let eventRequests = 0;
  await page.route("**/*", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;

    if (path === "/settings") {
      await route.fulfill({ json: settings() });
      return;
    }
    if (path === "/sessions") {
      const selectedAgent = url.searchParams.get("agent_id");
      const sessions =
        selectedAgent === agentA
          ? [session(sessionA, agentA, "web:alpha")]
          : selectedAgent === agentB
            ? [session(sessionB, agentB, "web:beta")]
            : [session(sessionA, agentA, "web:alpha"), session(sessionB, agentB, "web:beta")];
      await route.fulfill({ json: { sessions } });
      return;
    }
    if (path === "/session/" + sessionA) {
      await route.fulfill({ json: session(sessionA, agentA, "web:alpha") });
      return;
    }
    if (path === "/session/" + sessionB) {
      await route.fulfill({ json: session(sessionB, agentB, "web:beta") });
      return;
    }
    if (path === "/sessions/" + sessionA + "/messages") {
      await route.fulfill({
        json: { messages: options.messages(sessionA), last_event_id: null },
      });
      return;
    }
    if (path === "/sessions/" + sessionB + "/messages") {
      await route.fulfill({
        json: { messages: options.messages(sessionB), last_event_id: null },
      });
      return;
    }
    if (path.endsWith("/tool-calls")) {
      const sessionId = path.split("/")[2];
      await route.fulfill({ json: { session_id: sessionId, tool_calls: [] } });
      return;
    }
    if (path === "/runs" && request.method() === "POST") {
      await options.createRun?.();
      await route.fulfill({
        status: 500,
        json: { error: { message: "delayed create failure" } },
      });
      return;
    }
    if (path === "/runs" || (path.startsWith("/agents/") && path.endsWith("/runs"))) {
      await route.fulfill({ json: { runs: [] } });
      return;
    }
    if (path === "/events/session-activity") {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: "",
      });
      return;
    }
    if (path.endsWith("/events")) {
      eventRequests += 1;
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        headers: { "cache-control": "no-cache" },
        body: options.sessionEvents?.(path.split("/")[2], eventRequests) ?? "",
      });
      return;
    }

    await route.continue();
  });
}

test("agent switching cannot expose the previous agent's message entities", async ({ page }) => {
  await installRoutes(page, {
    messages: (id) => [textMessage(id === sessionA ? "alpha-only" : "beta-only")],
  });

  await page.goto(".");
  await expect(page.getByText("alpha-only", { exact: true })).toBeVisible();

  await page.locator("#agent-group-header-" + agentB).click();
  await expect(page.getByText("beta-only", { exact: true })).toBeVisible();
  await expect(page.getByText("alpha-only", { exact: true })).toHaveCount(0);

  const state = await page.evaluate(() => {
    const normalized = globalThis.__almsState?.state.value;
    return {
      visibleSessionId: normalized?.messages.visibleSessionId,
      texts: Object.values(normalized?.messages.byId ?? {}).map((message) => message.text),
    };
  });
  expect(state).toEqual({ visibleSessionId: sessionB, texts: ["beta-only"] });
});

test("a replay-gap reconnect converges to the authoritative message snapshot", async ({ page }) => {
  let messageRequests = 0;
  await installRoutes(page, {
    messages: () => {
      messageRequests += 1;
      return [textMessage(messageRequests === 1 ? "stale-before-gap" : "fresh-after-gap")];
    },
    sessionEvents: (_id, requestIndex) => {
      if (requestIndex !== 1) return "";
      const data = JSON.stringify({
        stream_epoch: streamEpoch,
        retained_from: 10,
        newest: 12,
        replay_gap: true,
        epoch_mismatch: false,
        requires_reconciliation: true,
      });
      return "id: 12\nevent: stream_state\ndata: " + data + "\n\n";
    },
  });

  await page.goto(".");
  await expect.poll(() => messageRequests).toBeGreaterThanOrEqual(2);
  await expect(page.getByText("fresh-after-gap", { exact: true })).toBeVisible();
  await expect(page.getByText("stale-before-gap", { exact: true })).toHaveCount(0);

  const texts = await page.evaluate(() => {
    const normalized = globalThis.__almsState?.state.value;
    const sessionId = normalized?.messages.visibleSessionId;
    return (sessionId ? normalized?.messages.idsBySession[sessionId] : [])?.map(
      (id) => normalized?.messages.byId[id]?.text,
    );
  });
  expect(texts).toEqual(["fresh-after-gap"]);
});

test("a delayed failed send cannot contaminate the newly selected session", async ({ page }) => {
  let releaseFailure = () => {};
  let createAttempts = 0;
  const failureGate = new Promise<void>((resolve) => {
    releaseFailure = resolve;
  });
  await installRoutes(page, {
    messages: (id) => [textMessage(id === sessionA ? "alpha-only" : "beta-only")],
    createRun: () => {
      createAttempts += 1;
      return failureGate;
    },
  });

  await page.goto(".");
  await expect(page.getByText("alpha-only", { exact: true })).toBeVisible();
  await page.getByLabel("Message input").fill("send from alpha");
  await page.getByLabel("Send message").click();
  await expect(page.getByText("send from alpha", { exact: true })).toBeVisible();

  // A second click before the first request has a run ID must be rejected
  // synchronously, leaving the operator's draft intact.
  await page.getByLabel("Message input").fill("second alpha draft");
  await page.getByLabel("Send message").click();
  await expect(page.getByLabel("Message input")).toHaveValue("second alpha draft");
  await expect.poll(() => createAttempts).toBe(1);

  await page.locator("#agent-group-header-" + agentB).click();
  await expect(page.getByText("beta-only", { exact: true })).toBeVisible();
  releaseFailure();

  await expect(page.getByText("Failed to start run: delayed create failure")).toHaveCount(0);
  await expect(page.locator(".thinking")).toHaveCount(0);
  const visible = await page.evaluate(() => {
    const normalized = globalThis.__almsState?.state.value;
    const sessionId = normalized?.messages.visibleSessionId;
    return {
      sessionId,
      texts: (sessionId ? normalized?.messages.idsBySession[sessionId] : [])?.map(
        (id) => normalized?.messages.byId[id]?.text,
      ),
    };
  });
  expect(visible).toEqual({ sessionId: sessionB, texts: ["beta-only"] });
});
