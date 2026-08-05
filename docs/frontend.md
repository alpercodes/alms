# Frontend architecture and migration

The browser UI is built with Preact, Vite, and strict TypeScript and embedded
into the Rust gateway. A validated compatibility boundary allows the remaining
JavaScript screens to migrate without changing the existing visual design.

## Layout

- `crates/alms-gateway/static/ui/` remains the editable UI source.
- `frontend/` contains strict TypeScript contracts, the mandatory runtime bridge,
  Vitest coverage, and Playwright smoke tests.
- `crates/alms-gateway/static/ui-dist/` is deterministic Vite output. It is
  committed because `rust-embed` needs assets during a Rust-only build.
- `crates/alms-gateway/src/server/routes.rs` embeds only `static/ui-dist/`.

The source `index.html` loads `typed-entry.ts` before the existing
`app.js`. The entry installs `globalThis.__almsContracts`, which lets
existing JavaScript opt into strict Zod validation without a flag-day rewrite.
The central API client and both SSE hooks decode raw JSON inside that guarded
boundary, then validate every payload before store actions mutate state.
All currently consumed REST surfaces and SSE event types have explicit schemas;
unknown routes and event types are rejected rather than bypassing the boundary.

Contract failures are logged, rejected, and shown in a visible alert. They do
not continue into state stores. Persisted SSE failures trigger an authoritative
REST reconciliation before the stream resumes beyond the rejected frame.

## Pinned toolchain

- Node.js is pinned in `.node-version`.
- npm is pinned by `packageManager` and the CI install step.
- Exact dependency versions live in `package-lock.json`.

ALMS is not deployed and does not require compatibility with an older frontend
build, so the exact versions in `package-lock.json` are the supported baseline.
Markdown sanitizing, signal-driven activity, normalized entity updates, and DM
collapsible behavior have focused regression coverage.

## Runtime dependency baseline

These are the packages that reach the browser. Bumping any of them is a
decision, not a drift — record the reasoning here when you do.

| Package | Version | Why this version |
|---|---|---|
| `preact` | 10.29.7 | Rendering core. Still 10.x; no major taken. |
| `@preact/signals` | 2.9.3 | State primitive. See the 2.x note below. |
| `htm` | 3.1.1 | Tagged-template JSX alternative; no build step for legacy screens. |
| `marked` | 18.0.6 | Markdown for assistant message bodies. See the 15 -> 18 note below. |
| `dompurify` | 3.4.12 | Sanitizes `marked` output. Minor bumps only; security-relevant, keep current. |
| `zod` | 4.4.3 | Schemas for the validated contract boundary. |

### Why `marked` is on 18.x rather than the 15.0.4 the CDN importmap pinned

Phase 5 (#1227) replaced the CDN importmap with a bundled, lockfile-pinned set
and moved `marked` across three majors in the same commit, unreviewed. #1232
audited that after the fact and kept the upgrade. The UI-relevant breaking
changes are:

- **v16** — packaging only: minified `lib/` output, `marked.min.js` and
  `lib/marked.cjs` removed, minimum Node raised to 20. We consume the ESM
  build through Vite on Node 22, so none of it reaches us.
- **v17** — list tokenizer rework: consecutive text tokens in lists, a
  simplified `listItem` renderer, checkbox tokens moved into the list
  tokenizer, loose-list text promoted to paragraph tokens. Rendering-relevant
  in principle. We override no renderers and call plain `marked.parse()`, so
  we only see the output shapes.
- **v18** — trailing blank lines are trimmed from block tokens, and the
  package moved to TypeScript 6.

Rendering output was diffed between 15.0.4 and 18.0.6 across ~45 markdown
inputs covering exactly those areas (tight/loose/ordered/nested lists, task
lists, lazy continuations, fenced and indented code, tables, blockquotes,
HTML blocks, trailing-blank-line variants). Three differences, all benign:

1. Loose task lists no longer emit a stray newline between the checkbox and
   its label (`<input …> \none` became `<input …> one`). HTML collapses that
   whitespace, so it was never visible — v18 is simply tidier.
2. Trailing blank lines after a raw HTML block are dropped. Whitespace-only.
3. `- ` on its own is now an empty list item instead of a literal `- `
   paragraph, which is the CommonMark-correct reading.

No regressions, so the upgrade stands. The important consumer to protect is
`utils/code-copy.js`, which parses `<pre><code class="language-x">…</code></pre>`
and strips the single trailing newline `marked` appends to fenced blocks —
precisely the kind of thing v18's block-token trimming could have removed. It
does not: the newline is still emitted, and `frontend/markdown-rendering.test.ts`
now pins that shape along with GFM soft breaks, task-list checkboxes surviving
sanitization, the `afterSanitizeAttributes` new-tab hook, and the XSS vectors.

### Why `@preact/signals` is on 2.x

Same origin — bundled at 2.9.3 where the importmap pinned 1.3.0. The
behavioral change that matters is that **2.x defers DOM updates by an
animation frame** instead of applying them synchronously. Two consequences:

- Tests that mutate a signal must await the flush (`waitFor`, or `act`).
  Asserting synchronously after `signal.value = …` will read stale DOM.
- There is no background-tab stall: the scheduler races
  `requestAnimationFrame` against a 35 ms `setTimeout`, so updates still land
  when rAF is throttled or absent.

The load-bearing consumer is the sidebar active-run dot in `session-list.js`.
That dot has churned repeatedly (#1211 -> #1216 -> #1220 -> #1225 -> #1226 ->
#1228) and nothing covered it; #1239 came close to pinning it permanently *on*
via an unreconciled `Queued` row, but that was caught in review and fixed
before merge. `frontend/sidebar-activity-reactivity.test.ts` now drives the
real production path — SSE activity event -> core store -> `backgroundRuns`
computed -> `SessionList` row class — and asserts a run starting on a
**non-selected** row lights that row, which is the exact #1211 symptom.
Verified passing under 2.9.3. The #1239 near-miss is the argument for it: an
expensive review pass caught what a cheap test would have caught earlier.

### Correction: the "read signals in the component body" rule

`session-list.js` used to explain its shape by claiming that reading the
signals inside the `hasActiveRun` helper left non-selected rows unsubscribed
under @preact/signals 1.3.0, and that hoisting the reads into the component
body was what fixed #1211. **That was a misdiagnosis, and it is corrected in
the code as of #1232.** Three things establish it:

- @preact/signals tracks dependencies dynamically for the whole render pass
  (`__r` enters a per-component effect, keyed on a global current-computation
  pointer). Read depth cannot matter by construction — any `.value` read while
  that pointer is set is captured, body or five frames down.
- The pre-#1216 code already read `bgRuns.value` during render, and for a
  non-selected row the `&&` short-circuited straight into it. The subscription
  the fix claimed to add was already there.
- #1220 later fixed the same symptom with the actual root cause: the sidebar
  subscribed only to the active agent's per-agent SSE feed, so activity on
  another agent's session never arrived. `bgRuns` was not failing to be
  observed — it was failing to change.

What survives is narrower and still worth keeping: a `.value` that is never
*evaluated* is never subscribed, so a `&&` short-circuit that skips a read
genuinely can drop a subscription. Evaluating the signals at the call site
guarantees all three are read on every render. That is a real hazard and
independent of where the read sits.

## Commands

```bash
npm ci
npm run ui:dev
npm run ui:check
npm run ui:build
npm run ui:test:e2e
```

`npm run ui:dev` proxies API and SSE paths to `http://127.0.0.1:8080` by
default. Set `ALMS_GATEWAY_URL` to point at a gateway on another origin.

`npm run ui:build` replaces `static/ui-dist/`. Commit the generated output
with its source change. CI rebuilds it and fails for tracked drift or omitted
untracked chunks. `make ci` installs the pinned frontend dependencies and runs
the non-browser frontend gates; Playwright is available as
`make frontend-test-e2e` and runs in the dedicated GitHub frontend job.

## State ownership and migration rule

New frontend code is strict TypeScript. Existing JavaScript migrates
screen-by-screen behind the mandatory runtime bridge. The typed core store owns
the authoritative normalized representations of agents, sessions, runs,
activity, messages, and jobs. Legacy screens may consume compatibility signals,
but server-entity writes go through named store actions. Snapshot-plus-buffer
reconciliation, lifecycle revisions, event cursors, and mutation generations
prevent stale network responses from regressing newer state.
