#!/usr/bin/env node
// Runs `cargo clippy` and `cargo test --workspace` on one commit of this repository and
// publishes the summary to the hub as test suite `cargo`, where the Request Tracking ops
// assistant reads it through the read-only MCP console (`hub_test_results {suite: "cargo"}`,
// RT-0287). Running a pull request's tests runs that code, so (owner's conditions, RT-0287):
//
// - the cargo processes get a scrubbed environment: PATH, HOME, the toolchain folders and the
//   locale only, never a token, key or SQL password (the admin token is read by this script for
//   the final PUT and never handed to a child);
// - they run with no network: dependencies are fetched first (`cargo fetch --locked`, no code of
//   the commit runs), then clippy and test run `--offline` in a new network namespace that has
//   only loopback (`unshare -rn`). Without namespaces the script refuses unless
//   `--no-isolation` is given, and the run then says `isolated: no`;
// - only counts and failure messages leave the machine.
//
//   node scripts/publish-cargo-run.mjs [--ref <sha|branch>] [--windows] [--dry-run] [--no-isolation]
//   AGENT_HUB_ADMIN_TOKEN=… [AGENT_HUB_ACCESS_CLIENT_ID=… AGENT_HUB_ACCESS_CLIENT_SECRET=…]
//   [AGENT_HUB_URL=https://agent.pms.id.vn] [TEST_RUNNER=imac|cloud]
//
// `--ref` checks the commit out in a temporary git worktree (the working tree is not touched);
// `--windows` also runs clippy for x86_64-pc-windows-gnu. Exit: 0 green, 1 clippy or a test
// failed, 2 the run or the publish could not happen.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const argv = process.argv.slice(2);
const flag = (f) => argv.includes(f);
const value = (f) => {
  const i = argv.indexOf(f);
  return i >= 0 ? argv[i + 1] : undefined;
};
const dryRun = flag('--dry-run');
const windows = flag('--windows');
const noIsolation = flag('--no-isolation');
const ref = value('--ref');
const hub = (process.env.AGENT_HUB_URL ?? 'https://agent.pms.id.vn').replace(/\/+$/, '');
const repo = resolve(new URL('..', import.meta.url).pathname);

/** The only variables a cargo child sees. */
const KEEP = ['PATH', 'HOME', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN', 'LANG', 'LC_ALL', 'TERM', 'TMPDIR'];
export function scrubbedEnv(env, targetDir) {
  const out = {};
  for (const k of KEEP) if (env[k] !== undefined) out[k] = env[k];
  out.CARGO_TARGET_DIR = targetDir;
  out.CARGO_TERM_COLOR = 'never';
  return out;
}

// Brings loopback up inside the new network namespace (tests use 127.0.0.1), no `ip` needed.
const LO_UP =
  "import fcntl,socket,struct;s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);" +
  "r=struct.pack('16sH14s',b'lo',0,bytes(14));f=struct.unpack('16sH14s',fcntl.ioctl(s,0x8913,r))[1];" +
  "fcntl.ioctl(s,0x8914,struct.pack('16sH14s',b'lo',f|1,bytes(14)))";

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8', maxBuffer: 512 * 1024 * 1024, ...opts });
  if (r.error) throw r.error;
  return r;
}

/** `cargo <args>` with the scrubbed environment, offline and without network when isolated. */
function cargo(cwd, env, args, isolated) {
  const full = offline(args);
  if (!isolated) return run('cargo', full, { cwd, env });
  return run('unshare', ['-rn', 'sh', '-c', `python3 -c "${LO_UP}" && exec cargo "$@"`, 'sh', ...full], { cwd, env });
}

/** `--offline` right after the subcommand: after `--` it would reach clippy-driver or the test binary. */
export function offline(args) {
  return [args[0], '--offline', ...args.slice(1)];
}

function canIsolate() {
  const r = spawnSync('unshare', ['-rn', 'sh', '-c', `python3 -c "${LO_UP}"`], { stdio: 'ignore' });
  return !r.error && r.status === 0;
}

/** Counts from cargo test's output: one "file" per test binary, failures by name. */
export function parseCargoTest(text) {
  const files = { passed: 0, failed: 0, skipped: 0 };
  const tests = { passed: 0, failed: 0, skipped: 0 };
  const failures = [];
  let current = '';
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const running = /^\s*(Running|Doc-tests)\s+(.*)$/.exec(line);
    if (running) current = running[2].replace(/\s*\(.*\)$/, '').trim();
    const result = /^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored/.exec(line);
    if (result) {
      tests.passed += Number(result[2]);
      tests.failed += Number(result[3]);
      tests.skipped += Number(result[4]);
      if (result[1] === 'ok') files.passed += 1;
      else files.failed += 1;
    }
    const failed = /^test (\S+) \.\.\. FAILED$/.exec(line);
    if (failed && failures.length < 50) {
      const header = lines.findIndex((l) => l === `---- ${failed[1]} stdout ----`);
      const body = [];
      for (let j = header + 1; header >= 0 && j < lines.length && body.length < 30; j++) {
        if (/^(---- .* ----|failures:|test result:)/.test(lines[j])) break;
        body.push(lines[j]);
      }
      const message = body.join('\n').trim();
      failures.push({ file: current.slice(0, 300), name: failed[1].slice(0, 500), message: message.slice(0, 2000) });
    }
  }
  return { files, tests, failures };
}

function main() {
  const isolated = !noIsolation;
  if (isolated && !canIsolate()) {
    process.stderr.write('cannot open a network namespace (unshare -rn): refusing to run code without isolation; pass --no-isolation to accept it\n');
    return 2;
  }
  const started = Date.now();
  let dir = repo;
  let worktree = null;
  if (ref) {
    worktree = mkdtempSync(join(tmpdir(), 'cargo-run-'));
    rmSync(worktree, { recursive: true, force: true });
    const fetch = run('git', ['-C', repo, 'fetch', '-q', 'origin', ref]);
    const add = run('git', ['-C', repo, 'worktree', 'add', '-q', '--detach', worktree, fetch.status === 0 ? 'FETCH_HEAD' : ref]);
    if (add.status !== 0) {
      process.stderr.write(`cannot check out ${ref}: ${add.stderr}\n`);
      return 2;
    }
    dir = worktree;
  }
  try {
    const commit = run('git', ['-C', dir, 'rev-parse', 'HEAD']).stdout.trim();
    const env = scrubbedEnv(process.env, join(repo, 'target', 'cargo-run'));
    const fetched = run('cargo', ['fetch', '--locked'], { cwd: dir, env });
    if (fetched.status !== 0) {
      process.stderr.write(`cargo fetch failed: ${fetched.stderr.slice(-1500)}\n`);
      return 2;
    }
    const clippyRuns = [['clippy', '--workspace', '--all-targets', '--', '-D', 'warnings']];
    if (windows) clippyRuns.push(['clippy', '--workspace', '--all-targets', '--target', 'x86_64-pc-windows-gnu', '--', '-D', 'warnings']);
    const clippyFailures = [];
    for (const args of clippyRuns) {
      const c = cargo(dir, env, args, isolated);
      if (c.status !== 0) {
        const errors = c.stderr.split('\n').filter((l) => /^(error|warning)/.test(l));
        clippyFailures.push({ file: '(clippy)', name: args.join(' ').slice(0, 500), message: (errors.join('\n') || c.stderr.slice(-2000)).slice(0, 2000) });
      }
    }
    const t = cargo(dir, env, ['test', '--workspace', '--no-fail-fast'], isolated);
    const parsed = parseCargoTest(`${t.stdout}\n${t.stderr}`);
    if (t.status !== 0 && parsed.tests.failed === 0 && parsed.files.failed === 0) {
      parsed.files.failed += 1;
      parsed.failures.push({ file: '(build)', name: 'cargo test', message: t.stderr.slice(-2000) });
    }
    const summary = {
      commit,
      branch: ref ?? (run('git', ['-C', dir, 'rev-parse', '--abbrev-ref', 'HEAD']).stdout.trim() || undefined),
      ran_at: Math.floor(started / 1000),
      duration_ms: Date.now() - started,
      command: `cargo clippy --workspace --all-targets -D warnings${windows ? ' (+windows-gnu)' : ''} && cargo test --workspace`,
      runner: `${process.env.TEST_RUNNER ?? 'local'}; isolated: ${isolated ? 'netns, no secrets' : 'no'}`,
      typecheck: clippyFailures.length ? 'fail' : 'pass',
      files: parsed.files,
      tests: parsed.tests,
      failures: [...clippyFailures, ...parsed.failures].slice(0, 50),
    };
    const failed = summary.typecheck === 'fail' || summary.tests.failed > 0 || summary.files.failed > 0;
    process.stdout.write(`${commit.slice(0, 12)}: clippy ${summary.typecheck}; binaries ${JSON.stringify(summary.files)}; tests ${JSON.stringify(summary.tests)}\n`);
    for (const f of summary.failures) process.stdout.write(`  FAILED ${f.file} ${f.name}\n`);
    if (dryRun) return failed ? 1 : 0;
    return publish(summary).then((ok) => (ok ? (failed ? 1 : 0) : 2));
  } finally {
    if (worktree && existsSync(worktree)) run('git', ['-C', repo, 'worktree', 'remove', '--force', worktree]);
  }
}

async function publish(summary) {
  const token = process.env.AGENT_HUB_ADMIN_TOKEN;
  if (!token) {
    process.stderr.write('AGENT_HUB_ADMIN_TOKEN is not set: nothing published\n');
    return false;
  }
  const headers = { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' };
  if (process.env.AGENT_HUB_ACCESS_CLIENT_ID) headers['CF-Access-Client-Id'] = process.env.AGENT_HUB_ACCESS_CLIENT_ID;
  if (process.env.AGENT_HUB_ACCESS_CLIENT_SECRET) headers['CF-Access-Client-Secret'] = process.env.AGENT_HUB_ACCESS_CLIENT_SECRET;
  const res = await fetch(`${hub}/admin/test-runs/cargo`, { method: 'PUT', headers, body: JSON.stringify(summary) });
  if (!res.ok) {
    process.stderr.write(`publish failed: HTTP ${res.status} ${(await res.text()).slice(0, 300)}\n`);
    return false;
  }
  process.stdout.write(`published to ${hub}: ${await res.text()}\n`);
  return true;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  Promise.resolve(main()).then((code) => process.exit(code));
}
