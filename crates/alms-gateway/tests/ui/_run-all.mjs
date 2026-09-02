#!/usr/bin/env node
// Runs every behaviour suite in this directory under Node's `node:test`
// runner. Entry point for `npm run ui:test:behavior` (and therefore for
// `npm run ui:test` / `npm run ui:check`).
//
// Why this exists (issue #7): these suites used to execute ONLY through
// `run_node_test` in `crates/alms-gateway/tests/ui_behavior.rs`, i.e. only
// under `cargo test -p alms-gateway`. `npm run ui:test` runs Vitest over
// `frontend/`, a different tree entirely — so a contributor could edit
// `static/ui/`, run `npm run ui:check`, see green, and have been validated by
// nothing that reads the tests written against their change. A frontend-only
// contributor has no other reason to invoke cargo at all.
//
// Why a script rather than a glob in `package.json`: `node --test` has TWO
// different exit-code behaviours for "the files aren't there", and only one of
// them is safe.
//
//   node --test "nothing-here-*.test.mjs"     -> exit 0   (glob, no matches)
//   node --test ./definitely-missing.test.mjs -> exit 1   (explicit path)
//
// Both verified on v22 and v24. So the obvious one-liner
//   node --test "crates/alms-gateway/tests/ui/*.test.mjs"
// would go green the moment that path drifted — "green means nothing"
// reintroduced by the fix for "green means nothing".
//
// Passing EXPLICIT PATHS harvested from `readdirSync` is therefore load-
// bearing, not merely tidy: it puts this runner on the safe side of both
// behaviours. The directory read cannot silently yield nothing (the assertion
// below), and a file that disappears between that read and the spawn aborts
// the run instead of quietly shrinking it. (Tim, PR #9.)
//
// Single source of truth: this script does not carry a list of suites. The
// DIRECTORY is the list. `ui_behavior.rs` keeps one `#[test]` per suite (for
// cargo-level granularity and the per-suite regression notes), and its
// `every_ui_test_file_has_a_cargo_test` guard fails if that hand-written set
// ever drifts from what is on disk. So a suite cannot be picked up by one
// runner and silently missed by the other.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import url from 'node:url';

const here = path.dirname(url.fileURLToPath(import.meta.url));

const files = fs
    .readdirSync(here)
    .filter((name) => name.endsWith('.test.mjs'))
    .sort()
    .map((name) => path.join(here, name));

if (files.length === 0) {
    console.error(
        `[ui:test:behavior] no *.test.mjs files found in ${here}\n`
        + 'Refusing to report success on an empty run — if the suites really '
        + 'were removed, update this script and ui_behavior.rs together.',
    );
    process.exit(1);
}

console.log(`[ui:test:behavior] running ${files.length} suites from ${here}`);

// `--test` runs each file in its own child process, so the import-rewriting
// harnesses in this directory (which write a stubbed copy of a `static/ui/`
// module to a temp dir and import that) stay isolated from each other exactly
// as they are when cargo invokes them one file at a time.
const child = spawn(process.execPath, ['--test', ...files], {
    stdio: 'inherit',
    cwd: here,
});

child.on('exit', (code, signal) => {
    if (signal) {
        console.error(`[ui:test:behavior] node --test terminated by ${signal}`);
        process.exit(1);
    }
    process.exit(code ?? 1);
});

child.on('error', (err) => {
    console.error('[ui:test:behavior] failed to spawn node --test:', err);
    process.exit(1);
});
