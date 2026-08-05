import { describe, expect, it } from "vitest";

import { renderMarkdown } from "../crates/alms-gateway/static/ui/deps.js";

/**
 * Rendering baseline for the `marked` + `DOMPurify` pipeline in `deps.js`.
 *
 * Every assistant message body in the chat pane is produced by
 * `renderMarkdown()`, but until #1232 nothing pinned its output — the
 * `marked` 15 -> 18 jump in #1227 (three majors) shipped on a green build
 * purely because no test looked at the HTML.
 *
 * These cases pin the output shapes that other code actually depends on:
 *
 *   - `breaks: true` / `gfm: true` from `marked.setOptions()`.
 *   - The `<pre><code class="language-x">…\n</code></pre>` shape that
 *     `utils/code-copy.js` parses for the clipboard payload — including
 *     the trailing newline it explicitly strips. `marked` v18 trims
 *     trailing blank lines from block tokens, so this is the assertion
 *     that would catch a future major silently dropping that newline.
 *   - GFM task-list `<input type="checkbox">` surviving sanitization.
 *   - The `afterSanitizeAttributes` hook rewriting links to open in a
 *     new tab, and DOMPurify stripping active content.
 *
 * Verified equal between `marked` 15.0.4 (the version the pre-#1227 CDN
 * importmap pinned) and 18.0.6 (bundled) — see `docs/frontend.md`.
 */
describe("markdown rendering baseline (marked + DOMPurify)", () => {
  it("honours the GFM soft-break option", () => {
    expect(renderMarkdown("first\nsecond")).toBe("<p>first<br>second</p>\n");
  });

  it("emits the fenced-code shape code-copy.js parses, with its trailing newline", () => {
    // `utils/code-copy.js` reads the inner <code> textContent and strips a
    // single trailing newline. Both halves of that contract are pinned here.
    expect(renderMarkdown("```js\nconst a = 1;\n```")).toBe(
      '<pre><code class="language-js">const a = 1;\n</code></pre>\n',
    );
  });

  it("keeps indented code blocks intact", () => {
    expect(renderMarkdown("    indented\n")).toBe("<pre><code>indented\n</code></pre>\n");
  });

  it("renders GFM task lists with checkboxes that survive sanitization", () => {
    const html = renderMarkdown("- [ ] todo\n- [x] done");

    expect(html).toContain('<input disabled="" type="checkbox">');
    expect(html).toContain('<input checked="" disabled="" type="checkbox">');
    expect(html).toContain("todo");
    expect(html).toContain("done");
  });

  it("renders tight and loose lists distinctly", () => {
    expect(renderMarkdown("- one\n- two")).toBe("<ul>\n<li>one</li>\n<li>two</li>\n</ul>\n");
    expect(renderMarkdown("- one\n\n- two")).toBe(
      "<ul>\n<li><p>one</p>\n</li>\n<li><p>two</p>\n</li>\n</ul>\n",
    );
  });

  it("renders GFM tables and strikethrough", () => {
    const table = renderMarkdown("| a | b |\n| - | - |\n| 1 | 2 |");
    expect(table).toContain("<table>");
    expect(table).toContain("<th>a</th>");
    expect(table).toContain("<td>1</td>");

    expect(renderMarkdown("~~gone~~")).toBe("<p><del>gone</del></p>\n");
  });

  it("opens rendered links in a new tab", () => {
    const html = renderMarkdown("[docs](https://example.com)");

    expect(html).toContain('href="https://example.com"');
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
  });

  it("strips script elements and inline event handlers", () => {
    expect(renderMarkdown("<script>alert(1)</script>")).toBe("");
    expect(renderMarkdown("<img src=x onerror=alert(2)>")).toBe('<img src="x">');
  });

  it("drops javascript: hrefs while keeping the link text", () => {
    const html = renderMarkdown("[x](javascript:alert(3))");

    expect(html).not.toContain("javascript:");
    expect(html).not.toContain("href");
    expect(html).toContain(">x</a>");
  });

  it("returns an empty string for empty input", () => {
    expect(renderMarkdown("")).toBe("");
  });
});
