import { expect, test } from "@playwright/test";

import { settingsFixture } from "./fixtures";

const agentId = "11111111-1111-4111-8111-111111111111";
const sessionId = "22222222-2222-4222-8222-222222222222";
const message =
  "Conversation summaries failed: no API key is set for OpenRouter (summary model: " +
  "google/gemma-4-31b-it). To fix this, change the summary model in Settings (Summary section, " +
  "or the Agents panel if this agent overrides it; unset it to use the agent's own model), or " +
  "add an OpenRouter key in Settings (API Keys).";

test("a summary_status on the session stream shows a banner that leads to Settings", async ({
  page,
}) => {
  await page.route("**/*", async (route) => {
    const path = new URL(route.request().url()).pathname;

    if (path === "/settings") {
      await route.fulfill({ json: settingsFixture(agentId) });
      return;
    }
    const session = {
      id: sessionId,
      agent_id: agentId,
      context_id: "web:summary-failure",
      session_type: "chat",
      has_active_run: false,
      created_at: "2026-07-12T10:00:00Z",
      updated_at: "2026-07-12T10:00:00Z",
    };
    if (path === "/sessions") {
      await route.fulfill({ json: { sessions: [session] } });
      return;
    }
    if (path === `/session/${sessionId}`) {
      await route.fulfill({ json: session });
      return;
    }
    if (path === `/sessions/${sessionId}/messages`) {
      await route.fulfill({ json: { messages: [], last_event_id: null } });
      return;
    }
    if (path === "/runs") {
      await route.fulfill({ json: { runs: [] } });
      return;
    }
    if (path === `/sessions/${sessionId}/events`) {
      const payload = {
        session_id: sessionId,
        failure: {
          code: "MISSING_KEY",
          provider: "openrouter",
          model: "google/gemma-4-31b-it",
          message,
        },
      };
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: `id: ephemeral-1\nevent: summary_status\ndata: ${JSON.stringify(payload)}\n\n`,
      });
      return;
    }
    if (path.includes("/events")) {
      await route.fulfill({ status: 200, contentType: "text/event-stream", body: "" });
      return;
    }
    await route.continue();
  });

  await page.goto(".");

  const banner = page.locator(".summary-failure-banner");
  await expect(banner).toContainText(message);
  await expect(page.locator("[data-alms-contract-boundary]")).toHaveCount(0);

  await expect(page.getByText("Summary provider")).toHaveCount(0);
  await banner.getByRole("button", { name: "Open Settings" }).click();
  await expect(page.getByText("Summary provider")).toHaveCount(1);
});
