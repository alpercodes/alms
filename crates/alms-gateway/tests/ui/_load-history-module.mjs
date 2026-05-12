// Shared loader for `static/ui/utils/history.js` used by Node-side unit
// tests. Mirrors the loader logic in `history.test.mjs` — we keep them
// independent (no cross-imports between test files) so each test file
// remains a self-contained unit that can be run in isolation, but the
// rewrite shape is identical so a future change to history.js's top-level
// imports breaks both loaders in parallel and points back here for the
// fix.
//
// The file is named with a leading underscore so node's test runner
// glob (which picks up *.test.mjs) ignores it.

import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const HISTORY_JS_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/history.js'
);

async function loadHistoryModule() {
    const src = fs.readFileSync(HISTORY_JS_PATH, 'utf8');

    const nextMsgIdImportRe =
        /^import\s+\{\s*nextMsgId\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!nextMsgIdImportRe.test(src)) {
        throw new Error(
            'history.js: expected a top-level `import { nextMsgId } from ...` line — '
            + 'test rewrite would not apply. Update _load-history-module.mjs '
            + 'if the import shape changed.'
        );
    }

    const constantsImportRe =
        /^import\s+\{\s*DM_END_REASON_LABELS\s*\}\s+from\s+['"][^'"]+['"];?\s*$/m;
    if (!constantsImportRe.test(src)) {
        throw new Error(
            'history.js: expected a top-level `import { DM_END_REASON_LABELS } from ...` line — '
            + 'test rewrite would not apply. Update _load-history-module.mjs '
            + 'if the import shape changed.'
        );
    }

    const stubbed = src
        .replace(
            nextMsgIdImportRe,
            'let __msgSeq = 0; function nextMsgId() { return "msg-" + (++__msgSeq); }'
        )
        .replace(
            constantsImportRe,
            'const DM_END_REASON_LABELS = { ignored: "no further replies", '
            + 'depth_exceeded: "message limit reached", '
            + 'user_cancelled: "cancelled by user", errored: "run failed" };'
        );

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-history-loader-'));
    const tmpFile = path.join(tmpDir, 'history.mjs');
    fs.writeFileSync(tmpFile, stubbed, 'utf8');

    return await import(url.pathToFileURL(tmpFile).href);
}

const mod = await loadHistoryModule();
export const mapHistoryMessages = mod.mapHistoryMessages;
export const groupDmReasoningBlocks = mod.groupDmReasoningBlocks;
