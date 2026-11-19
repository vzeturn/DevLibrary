import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { benchScript, LEGACY_SERVICE_PATTERN, psQuote } from '../src/install/benchScript';
import { importAesKey, seal, sha256Hex, unseal } from '../src/lib/crypto';
import { admin, b64, createTenant, enrollRequest, newKeys, setNow, T0, testEnv, type TestEnv } from './helpers';

const BASE = 'https://hub.test';
const SECRET = 'tok-S3CRET-value';
const FIXUP_SQL = "DISABLE TRIGGER TrgFolioSMILE_Send_HotelLink ON dbo.Folio;\nGO\nUPDATE dbo.Config SET Value = '0' WHERE Name = 'HLInventorySync';";
let env: TestEnv;

const bench = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
  tenant_id: 'bench-demo',
  site_id: 'test-pc',
  label: "Hiệp's bench",
  steps: [
    { setup: 'einvoice-enroll', args: ['--tenant', 'demo'], env: { DL_AGENT_SETUP_ARGS: `--token ${SECRET}` } },
    { setup: 'membership-enroll', args: ['--state-dir', '{{BENCH_DIR}}\\membership'], env: { DL_MEMBERSHIP_SQL: '{{SQL:SMILE_POS}}' } },
  ],
  sql_databases: ['SMILE_FO'],
  sql_fixups: [{ name: 'quiet HotelLink', database: 'SMILE_HL', sql_inline: FIXUP_SQL }],
  ...over,
});

const get = async (path: string): Promise<Response> => app.request(`${BASE}${path}`, {}, env);

async function create(over: Record<string, unknown> = {}): Promise<{ id: string; irm: string; expires_at: number; token: string }> {
  const res = await admin(env, 'POST', '/admin/bench', bench(over));
  expect(res.status).toBe(201);
  const body = (await res.json()) as { id: string; irm: string; expires_at: number };
  const token = /\/bench\/([A-Za-z0-9_-]+) \| iex$/.exec(body.irm)?.[1] ?? '';
  return { ...body, token };
}

function codeIn(script: string): string {
  return /^\$Code = '([^']+)'$/m.exec(script)?.[1] ?? '';
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv({ BENCH_KEY: b64(crypto.getRandomValues(new Uint8Array(32))) });
});
afterEach(() => vi.useRealTimers());

describe('POST /admin/bench', () => {
  it('creates a demo tenant, an enrolment code and a sealed bundle; answers the irm line once', async () => {
    const b = await create();
    expect(b.irm).toMatch(/^irm https:\/\/hub\.test\/bench\/[A-Za-z0-9_-]{43} \| iex$/);
    expect(b.id).toBe(await sha256Hex(b.token));
    expect(b.expires_at).toBe(T0 + 24 * 3600);

    const tenant = env.DB.raw.prepare('SELECT demo FROM tenants WHERE id = ?').get('bench-demo');
    expect(tenant).toEqual({ demo: 1 });
    const row = env.DB.raw.prepare('SELECT * FROM bench_bundles').get() as Record<string, unknown>;
    expect(row).toMatchObject({ id: b.id, site_id: 'test-pc', fetch_count: 0, max_fetches: 10, expires_at: T0 + 86400 });
    // Neither the token, the code nor a step secret is in D1 in clear.
    const dump = JSON.stringify(env.DB.raw.prepare('SELECT * FROM bench_bundles').all());
    expect(dump).not.toContain(b.token);
    expect(dump).not.toContain(SECRET);
    expect(dump).not.toContain('einvoice-enroll');
    expect(dump).not.toContain('HLInventorySync');
    const code = env.DB.raw.prepare('SELECT expires_at, used_at FROM enroll_codes WHERE id = ?').get(row.enroll_code_id as string);
    expect(code).toEqual({ expires_at: T0 + 86400, used_at: null });
  });

  it('sits behind the admin token and refuses to work without BENCH_KEY', async () => {
    const anon = await app.request(`${BASE}/admin/bench`, { method: 'POST', body: JSON.stringify(bench()) }, env);
    expect(anon.status).toBe(401);
    env = testEnv();
    const res = await admin(env, 'POST', '/admin/bench', bench());
    expect(res.status).toBe(503);
    expect(await res.json()).toEqual({ error: 'bench_key_missing' });
  });

  it('never turns a live tenant into a demo one', async () => {
    await createTenant(env, 'acme', false);
    const res = await admin(env, 'POST', '/admin/bench', bench({ tenant_id: 'acme' }));
    expect(res.status).toBe(409);
    expect(await res.json()).toEqual({ error: 'tenant_not_demo' });
    await createTenant(env, 'acme-demo', true);
    expect((await admin(env, 'POST', '/admin/bench', bench({ tenant_id: 'acme-demo' }))).status).toBe(201);
  });

  it.each([
    ['expires_hours over 72', { expires_hours: 73 }],
    ['a reserved sub-command', { steps: [{ setup: 'uninstall' }] }],
    ['a bad env name', { steps: [{ setup: 'x-enroll', env: { 'BAD-NAME': 'v' } }] }],
    ['a bad database placeholder', { steps: [{ setup: 'x-enroll', env: { A: '{{SQL:bad name}}' } }] }],
    ['a control character', { steps: [{ setup: 'x-enroll', args: ['a\nb'] }] }],
    ['no steps', { steps: [] }],
    ['a bad database name', { sql_databases: ['SMILE FO'] }],
    ['an SQL placeholder in args', { steps: [{ setup: 'x-enroll', args: ['{{SQL:SMILE_FO}}'] }] }],
    ['an unknown placeholder', { steps: [{ setup: 'x-enroll', env: { A: '{{HOME}}' } }] }],
    ['a fixup with both sources', { sql_fixups: [{ database: 'A', sql_url: 'https://x.test/a.sql', sql_inline: 'SELECT 1' }] }],
    ['a fixup with no source', { sql_fixups: [{ database: 'A' }] }],
    ['a fixup over http', { sql_fixups: [{ database: 'A', sql_url: 'http://x.test/a.sql' }] }],
    ['a sha256 without a URL', { sql_fixups: [{ database: 'A', sql_inline: 'SELECT 1', sha256: 'a'.repeat(64) }] }],
    ['a fixup with a NUL', { sql_fixups: [{ database: 'A', sql_inline: 'SELECT 1\u0000' }] }],
  ])('rejects %s', async (_name, over) => {
    expect((await admin(env, 'POST', '/admin/bench', bench(over))).status).toBe(400);
  });

  it('lists bundles with their status, never the sealed steps', async () => {
    const b = await create({ max_fetches: 1 });
    await get(`/bench/${b.token}`);
    const res = await admin(env, 'GET', '/admin/bench');
    const { bundles } = (await res.json()) as { bundles: Record<string, unknown>[] };
    expect(bundles).toHaveLength(1);
    expect(bundles[0]).toMatchObject({ id: b.id, label: "Hiệp's bench", fetch_count: 1, status: 'used_up', code_used_at: null });
    expect(bundles[0]).not.toHaveProperty('steps_enc');
  });
});

describe('GET /bench/:token', () => {
  it('serves the script, uncached, with an enrolment code that works on the demo (canary) tenant', async () => {
    const b = await create();
    const res = await get(`/bench/${b.token}`);
    expect(res.status).toBe(200);
    expect(res.headers.get('Content-Type')).toContain('text/plain');
    expect(res.headers.get('Cache-Control')).toBe('no-store');
    const script = await res.text();
    expect(script).toContain(`$Hub = '${BASE}'`);
    expect(script).toContain("$Label = 'Hiệp''s bench'");
    expect(script).toContain("$SqlDatabases = @('SMILE_FO')");
    expect(script).toContain("$SqlNames = @('SMILE_FO', 'SMILE_HL', 'SMILE_POS')");
    expect(script).toContain("'--state-dir', '{{BENCH_DIR}}\\membership'");
    const sql64 = Buffer.from(FIXUP_SQL, 'utf8').toString('base64');
    expect(script).toContain(`@{ Name = 'quiet HotelLink'; Database = 'SMILE_HL'; Url = ''; Sha256 = ''; Sql64 = '${sql64}' }`);
    expect(script).not.toContain('HLInventorySync');
    // The older per-product agents stay: install.ps1 runs without -Replace.
    expect(script).toMatch(/-Code \$Code -Channel canary\r?\n/);
    expect(script).not.toMatch(/-Replace\b/);
    expect(script).toContain(`'DL_AGENT_SETUP_ARGS' = '--token ${SECRET}'`);
    expect(script).toContain('-Code $Code -Channel canary');
    expect(script).not.toContain('__DATA__');

    const { publicKey } = await newKeys();
    const enrolled = await enrollRequest(env, {
      code: codeIn(script),
      public_key: publicKey,
      hostname: 'TEST-PC',
      os: 'windows',
      arch: 'x86_64',
      version: '1.0.0',
      modules: [],
    });
    expect(enrolled.status).toBe(200);
    expect(await enrolled.json()).toMatchObject({ tenant: 'bench-demo', site: 'test-pc', channel: 'canary' });
  });

  it('counts fetches and answers 404 once used up, expired, revoked or unknown', async () => {
    const b = await create({ max_fetches: 2, expires_hours: 1 });
    expect((await get(`/bench/${b.token}`)).status).toBe(200);
    expect((await get(`/bench/${b.token}`)).status).toBe(200);
    const third = await get(`/bench/${b.token}`);
    expect(third.status).toBe(404);
    expect(third.headers.get('Cache-Control')).toBe('no-store');
    expect(await third.text()).toContain('unknown, expired, revoked or used up');

    const c = await create({ expires_hours: 1 });
    setNow(T0 + 3600);
    expect((await get(`/bench/${c.token}`)).status).toBe(404);
    setNow(T0);

    expect((await get(`/bench/${'A'.repeat(43)}`)).status).toBe(404);
    expect((await get('/bench/short')).status).toBe(404);
  });

  it('revoking a bundle also revokes its unused code', async () => {
    const b = await create();
    expect((await admin(env, 'DELETE', `/admin/bench/${b.id}`)).status).toBe(200);
    expect((await get(`/bench/${b.token}`)).status).toBe(404);
    expect((await admin(env, 'DELETE', `/admin/bench/${b.id}`)).status).toBe(404);
    const code = env.DB.raw.prepare('SELECT revoked_at FROM enroll_codes').get();
    expect(code).toEqual({ revoked_at: T0 });
  });

  it('a sealed bundle moved to another row does not open (the id is the AAD)', async () => {
    const a = await create();
    const b = await create();
    const sealedA = (env.DB.raw.prepare('SELECT steps_enc FROM bench_bundles WHERE id = ?').get(a.id) as { steps_enc: string }).steps_enc;
    env.DB.raw.prepare('UPDATE bench_bundles SET steps_enc = ? WHERE id = ?').run(sealedA, b.id);
    const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const res = await get(`/bench/${b.token}`);
    expect(res.status).toBe(500);
    expect(errors).toHaveBeenCalledWith(expect.stringContaining('bench_unseal_failed'));
    errors.mockRestore();
  });
});

describe('seal / unseal', () => {
  it('round-trips and refuses a wrong key, a wrong AAD or a malformed value', async () => {
    const key = await importAesKey(b64(new Uint8Array(32).fill(7)));
    const other = await importAesKey(b64(new Uint8Array(32).fill(8)));
    if (!key || !other) throw new Error('key import failed');
    const sealed = await seal(key, 'hello', 'row-1');
    expect(await unseal(key, sealed, 'row-1')).toBe('hello');
    await expect(unseal(key, sealed, 'row-2')).rejects.toThrow();
    await expect(unseal(other, sealed, 'row-1')).rejects.toThrow();
    await expect(unseal(key, 'v1.nope', 'row-1')).rejects.toThrow('malformed');
    expect(await importAesKey(undefined)).toBeNull();
    expect(await importAesKey(b64(new Uint8Array(16)))).toBeNull();
  });
});

describe('bench script (PowerShell)', () => {
  it('recognises the older per-product agents and nothing else', () => {
    const legacy = new RegExp(LEGACY_SERVICE_PATTERN, 'i');
    for (const n of ['SmileHLAgent', 'SmilePmsAgent', 'MM.Arrival.Agent', 'MM.Membership.Agent', 'PdaSyncAgent', 'SRBackup', 'mmei-agent', 'Azolo E-Invoice Agent']) {
      expect(n).toMatch(legacy);
    }
    for (const n of ['dl-agent', 'Spooler', 'MSSQLSERVER', 'W32Time']) expect(n).not.toMatch(legacy);
    expect('SmilePmsDeviceAgent').toMatch(legacy);
  });

  it('doubles every quote character PowerShell honours', () => {
    expect(psQuote("a'b’c")).toBe("'a''b’’c'");
  });

  const pwsh = ((): boolean => {
    try {
      execFileSync('pwsh', ['-NoProfile', '-Command', '1'], { stdio: 'ignore' });
      return true;
    } catch {
      return false;
    }
  })();

  it.skipIf(!pwsh)('offers the SQL Server connection dl-agent already holds, server part only', () => {
    const script = benchScript({
      hub: BASE,
      label: 'x',
      tenant_id: 'bench-demo',
      site_id: 'test-pc',
      expires_at: T0,
      code: 'ABCD-EFGH-JKLM-NPQR',
      steps: [],
      sql_databases: ['SMILE_FO'],
      sql_fixups: [],
      sql_names: ['SMILE_FO'],
      sql_parts: false,
      remove_legacy: false,
      sql_grant_service: false,
    });
    expect(script).toContain('Keep it? (Y/n)');
    expect(script).toContain('Password (Enter keeps the saved one)');
    const lifted = /^# The server part of the connection[\s\S]*?(?=^# Asks for SQL Server)/m.exec(script)?.[0] ?? '';
    expect(lifted).toContain('function Get-SavedSql');
    const dir = mkdtempSync(join(tmpdir(), 'bench-sql-'));
    try {
      const probe = join(dir, 'probe.ps1');
      writeFileSync(
        probe,
        [
          '$St = @{ Secrets = (New-Object Collections.ArrayList) }',
          'function Add-Secret([string]$s) { if ($s) { [void]$St.Secrets.Add($s) } }',
          'function Warn([string]$m) { }',
          '$Data = $args[0]',
          lifted,
          '$s = Get-SavedSql',
          "@{ saved = $s; show = $(if ($s) { Show-Sql $s } else { '' }); secrets = $St.Secrets.Count } | ConvertTo-Json -Compress",
        ].join('\n'),
      );
      const run = (json: string | null): Record<string, unknown> => {
        const data = mkdtempSync(join(dir, 'data-'));
        if (json !== null) writeFileSync(join(data, 'sql.json'), json);
        return JSON.parse(execFileSync('pwsh', ['-NoProfile', '-File', probe, data]).toString()) as Record<string, unknown>;
      };
      const login = run(JSON.stringify({ databases: { SMILE_FO: { connection: 'Server="HOST\\SQL";User Id="sa";Password="p;w""x";Database="SMILE_FO";' } } }));
      expect(login.show).toBe('HOST\\SQL, SQL login sa');
      expect(login.saved).not.toMatch(/SMILE_FO|Catalog|Database/i);
      expect(login.saved).toContain('p;w"x');
      expect(login.secrets).toBe(1);
      const win = run(JSON.stringify({ databases: { SMILE_FO: { connection: 'Server=.;Integrated Security=true;Database=SMILE_FO;' } } }));
      expect(win.show).toBe('., Windows authentication');
      expect(run(JSON.stringify({ databases: { X: { connection: 'sqlserver://sa:pw@host/X' } } })).saved).toBe('');
      expect(run('{"databases":{}}').saved).toBe('');
      expect(run('not json').saved).toBe('');
      expect(run(null).saved).toBe('');
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it.skipIf(!pwsh)('parses clean in pwsh and its data block round-trips tricky values', () => {
    const tricky = `it's ‘quoted’ $(Get-Date) \`n "x" {{SQL:SMILE_FO}}`;
    const script = benchScript({
      hub: BASE,
      label: tricky,
      tenant_id: 'bench-demo',
      site_id: 'test-pc',
      expires_at: T0,
      code: 'ABCD-EFGH-JKLM-NPQR',
      steps: [
        { setup: 'einvoice-enroll', args: [tricky], env: { DL_AGENT_SETUP_ARGS: tricky } },
        { setup: 'arrival-enroll', args: [], env: {} },
      ],
      sql_databases: [],
      sql_fixups: [
        { name: tricky, database: 'SMILE_FO', sql_inline: `${tricky}\r\nGO\r\nSELECT N'Đã khóa'\n  go  \nSELECT 2\nGOTO x` },
        { database: 'SMILE_HL', sql_url: 'https://x.test/fix.sql', sha256: 'a'.repeat(64) },
      ],
      sql_names: ['SMILE_FO'],
      sql_parts: true,
      remove_legacy: true,
      sql_grant_service: true,
    });
    const dir = mkdtempSync(join(tmpdir(), 'bench-ps-'));
    try {
      const file = join(dir, 'bench.ps1');
      writeFileSync(file, script);
      const parse = `$e = $null; $t = $null; [void][System.Management.Automation.Language.Parser]::ParseFile('${file}', [ref]$t, [ref]$e); $e.Count`;
      expect(execFileSync('pwsh', ['-NoProfile', '-Command', parse]).toString().trim()).toBe('0');

      const data = script.slice(script.indexOf('$Hub = '), script.indexOf('$Product = '));
      const dataFile = join(dir, 'data.ps1');
      // The real Split-Batches, lifted from the script, on the decoded inline SQL.
      const split = /^function Split-Batches[\s\S]*?\n}\n/m.exec(script)?.[0] ?? '';
      expect(split).not.toBe('');
      writeFileSync(
        dataFile,
        [
          data,
          split,
          '$sql = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($Fixups[0].Sql64))',
          '$names = @(\'SmileHLAgent\', \'MM.Arrival.Agent\', \'PdaSyncAgent\', \'SRBackup Agent\', \'mmei-agent\', \'SmilePmsAgent\', \'MM.Membership.Agent\', \'dl-agent\', \'Spooler\', \'MSSQLSERVER\')',
          '@{ label = $Label; arg = $Steps[0].Argv[0]; env = $Steps[0].Env[\'DL_AGENT_SETUP_ARGS\']; steps = $Steps.Count; argv2 = $Steps[1].Argv.Count;',
          '   fixups = $Fixups.Count; fname = $Fixups[0].Name; sql = $sql; batches = @(Split-Batches $sql).Count; url = $Fixups[1].Url; sha = $Fixups[1].Sha256; sql64b = $Fixups[1].Sql64;',
          '   legacy = @($names | Where-Object { $_ -match $LegacyPattern }) } | ConvertTo-Json -Compress',
        ].join('\n'),
      );
      const out = JSON.parse(execFileSync('pwsh', ['-NoProfile', '-File', dataFile]).toString()) as Record<string, unknown>;
      expect(out).toEqual({
        label: tricky,
        arg: tricky,
        env: tricky,
        steps: 2,
        argv2: 0,
        fixups: 2,
        fname: tricky,
        sql: `${tricky}\r\nGO\r\nSELECT N'Đã khóa'\n  go  \nSELECT 2\nGOTO x`,
        batches: 3,
        url: 'https://x.test/fix.sql',
        sha: 'a'.repeat(64),
        sql64b: '',
        legacy: ['SmileHLAgent', 'MM.Arrival.Agent', 'PdaSyncAgent', 'SRBackup Agent', 'mmei-agent', 'SmilePmsAgent', 'MM.Membership.Agent'],
      });
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe('seal integration', () => {
  it('bundle ids are hex SHA-256 of the token', async () => {
    const b = await create();
    expect(b.id).toMatch(/^[0-9a-f]{64}$/);
  });
});
