// node --test scripts/release/build-password.test.mjs
// The release scripts refuse a dl-agent build without the SMILE default SQL password
// (1.3.63, 1.3.64 and 1.3.66 shipped without it, so pda-sync could not apply new grants).
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const lib = join(dirname(fileURLToPath(import.meta.url)), 'lib.sh');
const SECRET = 'pw-Secret-123';

/** Runs `script` in bash after sourcing lib.sh, with an operator config dir of its own. */
function run(script, { env = {}, configFile } = {}) {
  const cfg = mkdtempSync(join(tmpdir(), 'dl-rel-cfg-'));
  if (configFile !== undefined) writeFileSync(join(cfg, 'einvoice-build.env'), configFile);
  const r = spawnSync('bash', ['-c', `set -euo pipefail; . "$1"; ${script}`, '_', lib], {
    env: { PATH: process.env.PATH, HOME: cfg, DL_AGENT_CONFIG_DIR: cfg, ...env },
    encoding: 'utf8',
  });
  return { status: r.status, out: r.stdout, err: r.stderr, cfg };
}

function binary(dir, name, withSecret) {
  const f = join(dir, name);
  const body = Buffer.concat([Buffer.from([0, 1, 2, 255]), Buffer.from(withSecret ? `x${SECRET}y` : 'xy'), Buffer.from([0])]);
  writeFileSync(f, body);
  return f;
}

test('a release build refuses to start without the default SQL password', () => {
  const r = run('need_build_password; echo reached');
  assert.notEqual(r.status, 0);
  assert.doesNotMatch(r.out, /reached/);
  assert.match(r.err, /MMEI_BUILD_DEFAULT_SQL_PASSWORD is not set/);
  assert.match(r.err, /DL_BUILD_NO_DEFAULT_SQL_PASSWORD=1/);
});

test('the password comes from the operator config file and reaches child processes', () => {
  const r = run(`need_build_password; bash -c '[ "$MMEI_BUILD_DEFAULT_SQL_PASSWORD" = "${SECRET}" ] && echo exported'`, {
    configFile: `MMEI_BUILD_DEFAULT_SQL_PASSWORD=${SECRET}\n`,
  });
  assert.equal(r.status, 0, r.err);
  assert.equal(r.out.trim(), 'exported');
  assert.doesNotMatch(r.out + r.err, new RegExp(SECRET));
});

test('a variable set but not exported is exported for the build machine', () => {
  const r = run(`MMEI_BUILD_DEFAULT_SQL_PASSWORD=${SECRET}; need_build_password; bash -c '[ -n "\${MMEI_BUILD_DEFAULT_SQL_PASSWORD:-}" ] && echo seen'`);
  assert.equal(r.status, 0, r.err);
  assert.equal(r.out.trim(), 'seen');
});

test('the environment wins over the config file', () => {
  const r = run(`need_build_password; bash -c '[ "$MMEI_BUILD_DEFAULT_SQL_PASSWORD" = "${SECRET}" ] && echo env'`, {
    env: { MMEI_BUILD_DEFAULT_SQL_PASSWORD: SECRET },
    configFile: 'MMEI_BUILD_DEFAULT_SQL_PASSWORD=from-file\n',
  });
  assert.equal(r.status, 0, r.err);
  assert.equal(r.out.trim(), 'env');
});

test('DL_BUILD_NO_DEFAULT_SQL_PASSWORD=1 builds without it on purpose and says so', () => {
  const r = run('need_build_password; echo reached', { env: { DL_BUILD_NO_DEFAULT_SQL_PASSWORD: '1' } });
  assert.equal(r.status, 0, r.err);
  assert.equal(r.out.trim(), 'reached');
  assert.match(r.err, /warning: .*without the default SQL password/);
});

test('signing accepts binaries that carry the password', () => {
  const dir = mkdtempSync(join(tmpdir(), 'dl-rel-bin-'));
  const a = binary(dir, 'a.exe', true);
  const b = binary(dir, 'b', true);
  const r = run(`check_build_password "${a}" "${b}"; echo ok`, { env: { MMEI_BUILD_DEFAULT_SQL_PASSWORD: SECRET } });
  assert.equal(r.status, 0, r.err);
  assert.equal(r.out.trim(), 'ok');
  assert.doesNotMatch(r.out + r.err, new RegExp(SECRET));
});

test('signing refuses a binary built without the password and names it, never the password', () => {
  const dir = mkdtempSync(join(tmpdir(), 'dl-rel-bin-'));
  const good = binary(dir, 'good.exe', true);
  const bad = binary(dir, 'bad.exe', false);
  const r = run(`check_build_password "${good}" "${bad}"; echo signed`, { env: { MMEI_BUILD_DEFAULT_SQL_PASSWORD: SECRET } });
  assert.notEqual(r.status, 0);
  assert.doesNotMatch(r.out, /signed/);
  assert.match(r.err, /bad\.exe was built without MMEI_BUILD_DEFAULT_SQL_PASSWORD/);
  assert.doesNotMatch(r.out + r.err, new RegExp(SECRET));
});

test('the password never reaches argv of the check', () => {
  const dir = mkdtempSync(join(tmpdir(), 'dl-rel-bin-'));
  const f = binary(dir, 'a.exe', true);
  // A grep that records its arguments stands in for the real one.
  const shim = mkdtempSync(join(tmpdir(), 'dl-rel-shim-'));
  const log = join(shim, 'argv');
  writeFileSync(join(shim, 'grep'), `#!/usr/bin/env bash\nprintf '%s\\n' "$@" >>"${log}"\nexec /usr/bin/grep "$@"\n`, { mode: 0o755 });
  const r = run(`check_build_password "${f}"; cat "${log}"`, {
    env: { MMEI_BUILD_DEFAULT_SQL_PASSWORD: SECRET, PATH: `${shim}:${process.env.PATH}` },
  });
  assert.equal(r.status, 0, r.err);
  assert.match(r.out, /a\.exe/);
  assert.doesNotMatch(r.out, new RegExp(SECRET));
});

test('an intended build without the password skips the check', () => {
  const dir = mkdtempSync(join(tmpdir(), 'dl-rel-bin-'));
  const f = binary(dir, 'a.exe', false);
  const r = run(`check_build_password "${f}"; echo ok`, { env: { DL_BUILD_NO_DEFAULT_SQL_PASSWORD: '1' } });
  assert.equal(r.status, 0, r.err);
  assert.equal(r.out.trim(), 'ok');
});
