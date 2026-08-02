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
