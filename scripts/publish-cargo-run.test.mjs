// node --test scripts/publish-cargo-run.test.mjs
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { offline, parseCargoTest, scrubbedEnv } from './publish-cargo-run.mjs';

test('cargo children never see a secret', () => {
  const env = scrubbedEnv(
    { PATH: '/bin', HOME: '/h', RUSTUP_HOME: '/r', AGENT_HUB_ADMIN_TOKEN: 'a', GH_TOKEN: 'g', DL_AGENT_SQL_PASSWORD: 'p', AWS_SECRET_ACCESS_KEY: 'k' },
    '/t',
  );
  assert.deepEqual(env, { PATH: '/bin', HOME: '/h', RUSTUP_HOME: '/r', CARGO_TARGET_DIR: '/t', CARGO_TERM_COLOR: 'never' });
});

test('counts binaries and tests, names each failure with its message', () => {
  const out = [
    '     Running unittests src/lib.rs (target/debug/deps/dl_x-1)',
    'test a::b ... ok',
    'test a::c ... FAILED',
    'failures:',
    '',
    '---- a::c stdout ----',
    "thread 'a::c' panicked: boom",
    '',
    'failures:',
    '    a::c',
    '',
    'test result: FAILED. 1 passed; 1 failed; 2 ignored; 0 measured; 0 filtered out',
    '   Doc-tests dl_x',
    'test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out',
  ].join('\n');
  const r = parseCargoTest(out);
  assert.deepEqual(r.files, { passed: 1, failed: 1, skipped: 0 });
  assert.deepEqual(r.tests, { passed: 4, failed: 1, skipped: 2 });
  assert.deepEqual(r.failures, [{ file: 'unittests src/lib.rs', name: 'a::c', message: "thread 'a::c' panicked: boom" }]);
});

test('--offline goes to cargo, never after --', () => {
  assert.deepEqual(offline(['clippy', '--workspace', '--', '-D', 'warnings']), ['clippy', '--offline', '--workspace', '--', '-D', 'warnings']);
  assert.deepEqual(offline(['test', '--workspace']), ['test', '--offline', '--workspace']);
});
