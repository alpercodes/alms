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

/**
 * Sanitizer-configuration surface, pinned when `dompurify` moved
 * 3.4.12 -> 3.4.14 to clear GHSA-55q2-fjhq-7xh7 (#1250).
 *
 * The advisory needs two non-default preconditions: `IN_PLACE: true`, plus a
 * `beforeSanitizeElements` / `uponSanitizeElement` hook that detaches the
 * current node. `deps.js` uses neither — it calls `DOMPurify.sanitize(string)`
 * with no config, and its single `afterSanitizeAttributes` hook only adds
 * `target` / `rel` to anchors (pinned by the "opens rendered links in a new
 * tab" case above). ALMS was therefore never exposed.
 *
 * A 56-case differential run of the whole `renderMarkdown()` pipeline across
 * 3.4.12 and 3.4.14 found exactly one output change: the SVG `pointer-events`
 * and `vector-effect` presentation attributes were added to the allow-list in
 * 3.4.14. That single delta is pinned below, alongside the active-SVG cases
 * that must keep failing closed. Full reasoning in `docs/frontend.md`.
 */
describe("sanitizer configuration (dompurify)", () => {
  it("returns a sanitized string rather than a live DOM tree", () => {
    // One contract with two halves, and the exact-output assertion is what
    // carries the second:
    //
    //   * string out — no `RETURN_DOM` / `RETURN_DOM_FRAGMENT`, so the caller
    //     never holds the dirty tree. That is the structural reason
    //     GHSA-55q2-fjhq-7xh7 does not reach us.
    //   * still sanitizing, on stock defaults — `DOMPurify.sanitize()` is
    //     still in the path and nothing has been added to the allow-list.
    //
    // A `typeof` check alone only pins the first half: it still passes if
    // `renderMarkdown()` is reduced to `return raw`, or if someone adds
    // `ADD_TAGS: ['iframe']`. The exact match fails on both.
    const rendered = renderMarkdown(
      'text <iframe src="https://example.com"></iframe> <b onclick="alert(1)">bold</b> tail',
    );

    expect(typeof rendered).toBe("string");
    // The doubled space is the removed `<iframe>`; `onclick` is gone too.
    expect(rendered).toBe("<p>text  <b>bold</b> tail</p>\n");
  });

  it("allows the SVG presentation attributes added in dompurify 3.4.14", () => {
    // A version tripwire on a *third-party* allow-list, not an ALMS
    // requirement: it records the observed 3.4.12 -> 3.4.14 delta. If a later
    // DOMPurify tightens these attributes back, this goes red to force a look
    // rather than because anything of ours broke.
    const html = renderMarkdown(
      '<svg><rect pointer-events="none" vector-effect="non-scaling-stroke" ' +
        'fill="none" width="4" height="4"/></svg>',
    );

    expect(html).toContain('pointer-events="none"');
    expect(html).toContain('vector-effect="non-scaling-stroke"');
  });

  it("strips active content from inline SVG", () => {
    expect(renderMarkdown('<svg><script>alert(1)</script><circle r="1"/></svg>')).toBe(
      '<p><svg><circle r="1"></circle></svg></p>\n',
    );
    expect(
      renderMarkdown("<svg><foreignObject><img src=x onerror=alert(1)></foreignObject></svg>"),
    ).toBe("<p><svg></svg></p>\n");
    expect(
      renderMarkdown(
        '<svg><a><animate attributeName="href" values="javascript:alert(1)"/><text>x</text></a></svg>',
      ),
    ).toBe("<p><svg><a><text>x</text></a></svg></p>\n");
  });
});
