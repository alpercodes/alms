# Frontend build and migration

The browser UI is built with Vite and embedded into the Rust gateway. Phase 5
adds the typed build boundary without changing the existing visual design or
rewriting the legacy Preact modules.

## Layout

- `crates/alms-gateway/static/ui/` remains the editable UI source.
- `frontend/` contains strict TypeScript contracts, the compatibility bridge,
  Vitest coverage, and Playwright smoke tests.
- `crates/alms-gateway/static/ui-dist/` is deterministic Vite output. It is
  committed because `rust-embed` needs assets during a Rust-only build.
- `crates/alms-gateway/src/server/routes.rs` embeds only `static/ui-dist/`.

The source `index.html` loads `typed-entry.ts` before the existing
`app.js`. The entry installs `globalThis.__almsContracts`, which lets
existing JavaScript opt into strict Zod validation without a flag-day rewrite.
The central API client and both SSE hooks decode raw JSON inside that guarded
boundary, then validate every payload before existing handlers mutate state.
All currently consumed REST surfaces and SSE event types have explicit schemas;
only genuinely unknown future routes or event types use the object-shaped
compatibility fallback.

Contract failures are logged, rejected, and shown in a visible alert. They do
not continue into the legacy state stores.

## Pinned toolchain

- Node.js is pinned in `.node-version`.
- npm is pinned by `packageManager` and the CI install step.
- Exact dependency versions live in `package-lock.json`.

## Commands

```bash
npm ci
npm run ui:dev
npm run ui:check
npm run ui:build
npm run ui:test:e2e
```

`npm run ui:build` replaces `static/ui-dist/`. Commit the generated output
with its source change. CI rebuilds it and fails for tracked drift or omitted
untracked chunks. `make ci` installs the pinned frontend dependencies and runs
the non-browser frontend gates; Playwright is available as
`make frontend-test-e2e` and runs in the dedicated GitHub frontend job.

## Migration rule

New frontend code is strict TypeScript. Existing JavaScript migrates
screen-by-screen through the compatibility bridge. Phase 6 will move entity
ownership into normalized typed reducers; Phase 5 deliberately preserves the
current screen structure and behavior.
