import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const JOBS_TAB_PATH = path.resolve(
    __dirname,
    '../../static/ui/components/panel/jobs-tab.js'
);

function flattenTemplate(value) {
    if (value === null || value === undefined || value === false) return '';
    if (Array.isArray(value)) return value.map(flattenTemplate).join('');
    if (typeof value !== 'object') return String(value);
    if (!Array.isArray(value.strings) || !Array.isArray(value.values)) return '';
    let rendered = '';
    for (let index = 0; index < value.strings.length; index++) {
        rendered += value.strings[index];
        if (index < value.values.length) rendered += flattenTemplate(value.values[index]);
    }
    return rendered;
}

async function loadJobsTab(job) {
    const source = fs.readFileSync(JOBS_TAB_PATH, 'utf8');
    const imports = /^import[\s\S]*?from '\.\.\/\.\.\/utils\/format\.js';\s*/;
    assert.match(source, imports, 'expected jobs-tab import block');
    const stub = `
function html(strings, ...values) { return { strings: [...strings], values }; }
function useSignal(initial) { return { value: initial }; }
function useEffect() {}
const jobs = { value: [${JSON.stringify(job)}] };
function captureJobMutationGeneration() { return 0; }
function replaceJobs() {}
function createOptimisticJob() {}
function confirmOptimisticJobCreate() {}
function rollbackOptimisticJobCreate() {}
function cancelOptimisticJob() {}
function confirmOptimisticJobCancel() {}
function rollbackOptimisticJobCancel() {}
const agents = { value: [{ id: 'agent-1', name: 'Agent' }] };
const activeAgentId = { value: 'agent-1' };
const activePanelTab = { value: 'jobs' };
async function listJobs() { return []; }
async function createJob() { return {}; }
async function cancelJob() { return {}; }
function fmtDate(value) { return String(value); }
`;
    const rewritten = source.replace(imports, stub);
    const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'alms-jobs-tab-'));
    const tempFile = path.join(tempDir, 'jobs-tab.mjs');
    fs.writeFileSync(tempFile, rewritten, 'utf8');
    return import(url.pathToFileURL(tempFile).href);
}

test('failed jobs render terminal metadata without a cancel control', async () => {
    const { JobsTab } = await loadJobsTab({
        id: 'job-1',
        agent_id: 'agent-1',
        prompt: 'durable work',
        schedule: { type: 'recurring', cron: '* * * * *' },
        status: 'failed',
        terminal_reason: 'retry_exhausted',
        retry_count: 3,
        last_error: 'provider unavailable',
        next_run_at: null,
        last_run_at: '2026-07-14T10:00:00Z',
    });

    let tree;
    assert.doesNotThrow(() => {
        tree = JobsTab();
    });
    const rendered = flattenTemplate(tree).replace(/\s+/g, ' ');
    assert.match(rendered, /failed \(retry exhausted\)/);
    assert.match(rendered, /3 dispatch retries/);
    assert.match(rendered, /last error: provider unavailable/);
    assert.doesNotMatch(rendered, />Cancel</);
});

test('completed jobs render without a cancel control', async () => {
    const { JobsTab } = await loadJobsTab({
        id: 'job-2',
        agent_id: 'agent-1',
        prompt: 'one shot',
        schedule: { type: 'once', run_at: '2026-07-14T10:00:00Z' },
        status: 'completed',
        terminal_reason: 'completed',
        retry_count: 0,
        last_error: null,
        next_run_at: null,
        last_run_at: '2026-07-14T10:00:00Z',
    });

    const rendered = flattenTemplate(JobsTab()).replace(/\s+/g, ' ');
    assert.match(rendered, /completed \(completed\)/);
    assert.doesNotMatch(rendered, />Cancel</);
});
