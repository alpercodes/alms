import { render } from "@testing-library/preact";
import { beforeEach, describe, expect, it } from "vitest";

import type { renderParams as renderParamsFn } from "../crates/alms-gateway/static/ui/components/chat/tool-row.js";
import { installStateBridge } from "./state/bridge";

/**
 * The request row of a `workspace_write` tool call must not invent a `mode`
 * the caller did not send.
 *
 * `mode` is optional, and since #1305 an omitted one resolves *per file* on
 * the server (`WorkspaceFile::default_write_mode` — `append` for memories,
 * `write` for the other three). This renderer used to fabricate `write`
 * whenever `params.mode` was absent, which put "write" in the Workspace row
 * next to the "append" the Result row read from the tool's own `result.mode`
 * for the same call. Mirroring the backend default in JS would fix the
 * disagreement by creating a second source of truth that goes stale the next
 * time the default moves; rendering nothing instead leaves the Result pill as
 * the single answer to which branch ran.
 *
 * Structurally the same hole #1305 closed on the Rust side: the explicit case
 * renders a pill under either implementation, so only the *omitted* case can
 * tell "render what you were given" apart from "guess a default".
 */
describe("workspace_write request-mode pill", () => {
  let renderParams: typeof renderParamsFn;

  beforeEach(async () => {
    // `tool-row.js` transitively imports `state/entity-state.js`, which
    // throws at module scope without the bridge. Same order as
    // `subagent-session-label.test.ts`: install, then dynamic-import.
    installStateBridge();
    ({ renderParams } =
      await import("../crates/alms-gateway/static/ui/components/chat/tool-row.js"));
  });

  /** Text of the mode pill in the Workspace row, or `null` when none is rendered. */
  function modePill(params: Record<string, unknown>): string | null {
    const vnode = renderParams("workspace_write", params);
    if (!vnode) throw new Error("workspace_write params did not render");
    const { container } = render(vnode);
    const row = container.querySelector(".tc-status-row");
    if (!row) throw new Error("no status row rendered");
    return row.querySelector(".tc-kv-meta")?.textContent ?? null;
  }

  it("renders the mode the caller sent", () => {
    expect(modePill({ file: "memories", content: "- a fact", mode: "append" })).toBe("append");
    expect(modePill({ file: "goals", content: "ship it", mode: "write" })).toBe("write");
  });

  it("renders no mode pill when the caller omitted one", () => {
    // The mirror of the row above, and the only one that fails if this
    // renderer goes back to guessing: an omitted `mode` is resolved
    // server-side and per file, so the request row cannot know it.
    expect(modePill({ file: "memories", content: "- a fact" })).toBeNull();
    expect(modePill({ file: "goals", content: "ship it" })).toBeNull();
  });

  it("still renders the target file either way", () => {
    const vnode = renderParams("workspace_write", { file: "memories", content: "- a fact" });
    if (!vnode) throw new Error("workspace_write params did not render");
    const { container } = render(vnode);
    expect(container.querySelector(".tc-kv-badge")?.textContent).toBe("memories");
  });
});
