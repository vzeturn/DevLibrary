#!/usr/bin/env node
// Runs the hub checks (typecheck + vitest) and publishes the summary to the hub, where the
// Request Tracking ops assistant reads it through the read-only MCP console (`hub_test_results`,
// RT-0300). Only counts and failure messages leave the machine.
//
//   AGENT_HUB_ADMIN_TOKEN=… [AGENT_HUB_ACCESS_CLIENT_ID=… AGENT_HUB_ACCESS_CLIENT_SECRET=…] \
//     [AGENT_HUB_URL=https://agent.pms.id.vn] [TEST_RUNNER=imac|cloud] pnpm test:publish [--dry-run]
//
// Exit code: the test result (1 when a test or the typecheck failed, 2 when publishing failed).
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const dryRun = process.argv.includes('--dry-run');
const hub = (process.env.AGENT_HUB_URL ?? 'https://agent.pms.id.vn').replace(/\/+$/, '');
const pnpm = process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm';

function run(cmd, args) {
  const r = spawnSync(cmd, args, { stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8' });
  if (r.error) throw r.error;
  return r;
}

function git(args) {
  const r = run('git', args);
  return r.status === 0 ? r.stdout.trim() : '';
}

const started = Date.now();
const typecheck = run(pnpm, ['-s', 'typecheck']).status === 0 ? 'pass' : 'fail';
const dir = mkdtempSync(join(tmpdir(), 'hub-tests-'));
const out = join(dir, 'vitest.json');
const vitest = run(pnpm, ['-s', 'vitest', 'run', '--reporter=json', `--outputFile=${out}`]);
let report;
try {
  report = JSON.parse(readFileSync(out, 'utf8'));
} catch (err) {
  process.stderr.write(`vitest wrote no report (exit ${vitest.status}): ${err.message}\n${vitest.stderr.slice(-2000)}\n`);
  process.exit(1);
} finally {
  rmSync(dir, { recursive: true, force: true });
}

const files = { passed: 0, failed: 0, skipped: 0 };
const tests = { passed: 0, failed: 0, skipped: 0 };
const failures = [];
for (const file of report.testResults ?? []) {
  const name = file.name.replace(process.cwd() + '/', '');
  const asserts = file.assertionResults ?? [];
  for (const a of asserts) {
    if (a.status === 'passed') tests.passed += 1;
    else if (a.status === 'failed') {
      tests.failed += 1;
      if (failures.length < 50) failures.push({ file: name, name: a.fullName.slice(0, 500), message: (a.failureMessages ?? []).join('\n').slice(0, 2000) });
    } else tests.skipped += 1;
  }
  if (file.status === 'failed') {
    files.failed += 1;
    if (asserts.length === 0 && failures.length < 50) failures.push({ file: name, name: '(file)', message: String(file.message ?? 'failed to load').slice(0, 2000) });
  } else if (asserts.length > 0 && asserts.every((a) => a.status !== 'passed' && a.status !== 'failed')) files.skipped += 1;
  else files.passed += 1;
}

const summary = {
  commit: git(['rev-parse', 'HEAD']) || '0000000',
  branch: git(['rev-parse', '--abbrev-ref', 'HEAD']) || undefined,
  ran_at: Math.floor(started / 1000),
  duration_ms: Date.now() - started,
  command: 'pnpm typecheck && pnpm test',
  runner: process.env.TEST_RUNNER ?? 'local',
  typecheck,
  files,
  tests,
  failures,
};
const failed = typecheck === 'fail' || tests.failed > 0 || files.failed > 0;
process.stdout.write(`typecheck ${typecheck}; files ${JSON.stringify(files)}; tests ${JSON.stringify(tests)}\n`);
if (dryRun) process.exit(failed ? 1 : 0);

const token = process.env.AGENT_HUB_ADMIN_TOKEN;
if (!token) {
  process.stderr.write('AGENT_HUB_ADMIN_TOKEN is not set: nothing published\n');
  process.exit(2);
}
const headers = { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' };
if (process.env.AGENT_HUB_ACCESS_CLIENT_ID) headers['CF-Access-Client-Id'] = process.env.AGENT_HUB_ACCESS_CLIENT_ID;
if (process.env.AGENT_HUB_ACCESS_CLIENT_SECRET) headers['CF-Access-Client-Secret'] = process.env.AGENT_HUB_ACCESS_CLIENT_SECRET;
const res = await fetch(`${hub}/admin/test-runs/agent-hub`, { method: 'PUT', headers, body: JSON.stringify(summary) });
if (!res.ok) {
  process.stderr.write(`publish failed: HTTP ${res.status} ${(await res.text()).slice(0, 300)}\n`);
  process.exit(2);
}
process.stdout.write(`published to ${hub}: ${await res.text()}\n`);
process.exit(failed ? 1 : 0);
