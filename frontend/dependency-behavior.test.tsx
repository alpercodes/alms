import { render, screen, waitFor } from "@testing-library/preact";
import { signal } from "@preact/signals";
import { h } from "preact";
import { describe, expect, it } from "vitest";

import { renderMarkdown } from "../crates/alms-gateway/static/ui/deps.js";

describe("pinned frontend dependency behavior", () => {
  it("renders GFM line breaks and sanitizes active content", () => {
    const html = renderMarkdown(
      "first\nsecond\n\n[docs](https://example.com)<script>alert(1)</script><img src=x onerror=alert(2)>",
    );

    expect(html).toContain("first<br>second");
    expect(html).not.toContain("<script");
    expect(html).not.toContain("onerror");
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
  });

  it("keeps Signals updates subscribed through Preact rendering", async () => {
    const active = signal(false);
    function ActivityProbe() {
      return h("span", null, active.value ? "active" : "idle");
    }

    render(h(ActivityProbe, null));
    expect(screen.getByText("idle")).toBeInTheDocument();
    active.value = true;
    await waitFor(() => expect(screen.getByText("active")).toBeInTheDocument());
  });
});
