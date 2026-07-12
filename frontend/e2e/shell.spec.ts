import { expect, test } from "@playwright/test";

import { settingsFixture } from "./fixtures";

const agentId = "11111111-1111-4111-8111-111111111111";
const sessionId = "22222222-2222-4222-8222-222222222222";

test("loads the existing dashboard through the bundled typed entrypoint", async ({ page }) => {
  await page.route("**/*", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;

    if (path === "/settings") {
      await route.fulfill({
        json: settingsFixture(agentId),
      });
      return;
    }
    if (path === "/sessions") {
      await route.fulfill({
        json: {
          sessions: [
            {
              id: sessionId,
              agent_id: agentId,
              context_id: "web:phase5-smoke",
              session_type: "chat",
              has_active_run: false,
              created_at: "2026-07-12T10:00:00Z",
              updated_at: "2026-07-12T10:00:00Z",
            },
          ],
        },
      });
      return;
    }
    if (path === `/session/${sessionId}`) {
      await route.fulfill({
        json: {
          id: sessionId,
          agent_id: agentId,
          context_id: "web:phase5-smoke",
          session_type: "chat",
          has_active_run: false,
        },
      });
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
    if (path.includes("/events")) {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: "",
      });
      return;
    }

    await route.continue();
  });

  await page.goto(".");

  await expect(page).toHaveTitle(/ALMS - atlas/);
  await expect(page.locator("#app")).not.toBeEmpty();
  await expect(page.getByText("atlas", { exact: false }).first()).toBeVisible();
  await expect(page.locator("[data-alms-contract-boundary]")).toHaveCount(0);
});
