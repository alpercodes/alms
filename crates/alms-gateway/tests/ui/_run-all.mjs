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
// Why a script rather than a glob in `package.json`: `node --test` EXITS 0
// WHEN ITS GLOB MATCHES NOTHING (verified on v22 and v24). A one-liner like
//   node --test "crates/alms-gateway/tests/ui/*.test.mjs"
// would therefore go green the moment the path drifted — the same
// "green means nothing" bug in a new costume. Discovery here is an explicit
// directory read with a non-empty assertion, so a broken path fails loudly.
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
