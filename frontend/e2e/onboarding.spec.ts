/**
 * First-run onboarding (issue #162 half A).
 *
 * The `hasStoredKey` behaviour suite covers the predicate. The invariant that
 * actually protects operators — **the key step is an offer, never a gate** —
 * lives in the components, and nothing pinned it: a change that disabled the
 * skip button when no key is stored (precisely what the module docs warn
 * against) left all 512 behaviour tests green. These specs drive the real
 * bundle so that mistake fails.
 *
 * Why an offer and not a gate: `GET /auth/keys` reports only the secrets store
 * (`list_keys`, auth_keys.rs), so `configured: false` is indistinguishable
 * between "no key anywhere" and a key wired into `alms.toml` through
 * `[llm.providers.<name>].api_key_env`, which resolves at gateway startup and
 * survives per-run re-resolution. The step may skip itself when a key IS
 * stored; it may never require one when none is.
 *
 * `OnboardingView` renders only when `agents` is empty, hence
 * `settingsFixture(id, { withAgent: false })`.
 */
import { expect, test, type Page } from "@playwright/test";

import { settingsFixture } from "./fixtures";

const agentId = "11111111-1111-4111-8111-111111111111";
const createdAgentId = "44444444-4444-4444-8444-444444444444";

/** Every slot `GET /auth/keys` emits — `VALID_PROVIDERS`, telegram included. */
const AUTH_SLOTS = ["openai", "anthropic", "openrouter", "gemini", "telegram"];

function keysPayload(configured: readonly string[]) {
  return {
    keys: AUTH_SLOTS.map((provider) => {
      const isSet = configured.includes(provider);
      return {
        provider,
        configured: isSet,
        key: isSet ? "sk-...abcd" : null,
        source: isSet ? "secrets" : "none",
      };
    }),
  };
}

interface OnboardingRoutes {
  /** Providers reporting `configured: true`. */
  readonly configured?: readonly string[];
  /** Fail the `/auth/keys` probe instead of answering it. */
  readonly failProbe?: boolean;
  /** Called when `POST /agents` is issued. */
  readonly onCreateAgent?: (body: unknown) => void;
}

async function installRoutes(page: Page, options: OnboardingRoutes = {}) {
  const { configured = [], failProbe = false, onCreateAgent } = options;

  await page.route("**/*", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;

    if (path === "/settings") {
      await route.fulfill({ json: settingsFixture(agentId, { withAgent: false }) });
      return;
    }
    if (path === "/auth/keys" && request.method() === "GET") {
      if (failProbe) {
        await route.abort();
        return;
      }
      await route.fulfill({ json: keysPayload(configured) });
      return;
    }
    if (path === "/auth/keys" && request.method() === "PUT") {
      await route.fulfill({ json: { ok: true, provider: "openrouter", key: "sk-...abcd" } });
      return;
    }
    if (path === "/agents" && request.method() === "POST") {
      onCreateAgent?.(request.postDataJSON());
      await route.fulfill({ json: agentRecord() });
      return;
    }
    if (path === "/agents") {
      await route.fulfill({ json: { agents: [agentRecord()] } });
      return;
    }
    if (path === "/sessions") {
      await route.fulfill({ json: { sessions: [] } });
      return;
    }
    if (path === "/runs") {
      await route.fulfill({ json: { runs: [] } });
      return;
    }
    if (path.includes("/events")) {
      await route.fulfill({ status: 200, contentType: "text/event-stream", body: "" });
      return;
    }

    await route.continue();
  });
}

function agentRecord() {
  return {
    id: createdAgentId,
    name: "smoke",
    description: "",
    has_telegram: false,
    debug_mode: false,
    is_default: true,
  };
}

const keyStep = (page: Page) => page.getByText("Step 1 of 2");
const skipButton = (page: Page) => page.locator(".onboard-skip");
const nameInput = (page: Page) => page.getByPlaceholder("my-agent");

test("no stored key: the key step is offered and the skip button works", async ({ page }) => {
  await installRoutes(page, { configured: [] });
  await page.goto(".");

  await expect(keyStep(page)).toBeVisible();
  await expect(page.getByRole("heading", { name: "Welcome to ALMS" })).toBeVisible();

  // The copy carries three claims the operator acts on. Pin them: OpenRouter
  // is the recommendation, and one key there covers BOTH compiled defaults.
  const card = page.locator(".onboard-card");
  await expect(card).toContainText("z-ai/glm-5.2");
  await expect(card).toContainText("google/gemma-4-31b-it");
  await expect(card).toContainText("no restart");

  // The invariant. Not merely present — ENABLED, with no key stored. This is
  // the assertion that fails if someone "tightens" the step into a gate.
  await expect(skipButton(page)).toBeVisible();
  await expect(skipButton(page)).toBeEnabled();

  await skipButton(page).click();
  await expect(nameInput(page)).toBeVisible();
  await expect(page.getByText("Step 2 of 2")).toBeVisible();
});

test("no stored key: skipping still lets the agent be created", async ({ page }) => {
  // "Agent creation is not gated on the key" stated as a request, not as a
  // rendering detail: POST /agents must be reachable from a session that
  // never supplied one.
  let created: unknown = null;
  await installRoutes(page, { configured: [], onCreateAgent: (body) => (created = body) });
  await page.goto(".");

  await skipButton(page).click();
  await nameInput(page).fill("smoke");
  await page.getByRole("button", { name: "Create Agent" }).click();

  await expect.poll(() => created).toEqual({ name: "smoke", is_default: true });
});

test("stored key: the key step is skipped entirely", async ({ page }) => {
  await installRoutes(page, { configured: ["openrouter"] });
  await page.goto(".");

  await expect(nameInput(page)).toBeVisible();
  await expect(skipButton(page)).toHaveCount(0);
  await expect(keyStep(page)).toHaveCount(0);
  // No step marker at all for a flow that only ever had one step.
  await expect(page.getByText("Step 2 of 2")).toHaveCount(0);
});

test("stored telegram token alone does not skip the key step", async ({ page }) => {
  // Regression for the bug Tim caught in PR #163. `telegram` is in
  // `VALID_PROVIDERS` as a channel bot token, so `GET /auth/keys` reports it
  // alongside the LLM slots — but it authenticates nothing an agent can think
  // with. An operator who wired up Telegram first used to have this step
  // skipped and land on exactly the failed first run #162 is about.
  await installRoutes(page, { configured: ["telegram"] });
  await page.goto(".");

  await expect(keyStep(page)).toBeVisible();
  await expect(skipButton(page)).toBeEnabled();
});

test("a failed /auth/keys probe shows the key step, not a dead end", async ({ page }) => {
  // The probe can only ever REMOVE a step. Every failure mode — network
  // error, 5xx, a contract-bridge rejection — lands in the same catch and
  // must leave the operator with a usable step 1.
  await installRoutes(page, { failProbe: true });
  await page.goto(".");

  await expect(keyStep(page)).toBeVisible();
  await expect(skipButton(page)).toBeEnabled();
  await skipButton(page).click();
  await expect(nameInput(page)).toBeVisible();
});
