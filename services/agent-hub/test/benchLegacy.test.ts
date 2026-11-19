import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { usesSqlParts } from '../src/domain/bench';
import { LEGACY_BODY, LEGACY_SERVICE_PATTERN, SQL_BINARY_PATTERN, SQL_SERVICE_PATTERN } from '../src/install/benchLegacy';
import { benchScript, psQuote } from '../src/install/benchScript';
import { importAesKey, seal } from '../src/lib/crypto';
import { admin, b64, setNow, T0, testEnv, type TestEnv } from './helpers';

const BASE = 'https://hub.test';
let env: TestEnv;

const EINVOICE_STEP = {
  setup: 'einvoice-enroll',
  args: ['--platform-url', 'https://einvoice.test'],
  env: {
    DL_AGENT_EINVOICE_TOKEN: 'tok-xyz',
    MMEI_SQL_HOST: '{{SQL_SERVER}}',
    MMEI_SQL_USER: '{{SQL_USER}}',
    MMEI_SQL_PASSWORD: '{{SQL_PASSWORD}}',
    MMEI_SQL_AUTH: '{{SQL_AUTH}}',
  },
};

const body = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
  tenant_id: 'bench-demo',
  site_id: 'test-pc',
  label: 'bench',
  steps: [EINVOICE_STEP],
  ...over,
});

async function scriptFor(over: Record<string, unknown> = {}): Promise<{ script: string; token: string }> {
  const res = await admin(env, 'POST', '/admin/bench', body(over));
  expect(res.status).toBe(201);
  const { irm } = (await res.json()) as { irm: string };
  const token = /\/bench\/([A-Za-z0-9_-]+) \| iex$/.exec(irm)?.[1] ?? '';
  const got = await app.request(`${BASE}/bench/${token}`, {}, env);
  expect(got.status).toBe(200);
  return { script: await got.text(), token };
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv({ BENCH_KEY: b64(crypto.getRandomValues(new Uint8Array(32))) });
});
afterEach(() => vi.useRealTimers());

describe('bench: remove_legacy and SQL part placeholders', () => {
  it('new bundles remove the older agents by default; remove_legacy false only lists them', async () => {
    const on = await scriptFor();
    expect(on.script).toContain('$RemoveLegacy = $true');
    expect(on.script).toContain('$SqlParts = $true');
    expect(on.script).toContain('function Remove-Legacy');
    expect(on.script).not.toContain('__LEGACY__');
    const off = await scriptFor({ remove_legacy: false, steps: [{ setup: 'x-enroll' }] });
    expect(off.script).toContain('$RemoveLegacy = $false');
    expect(off.script).toContain('$SqlParts = $false');
  });

  it('a bundle sealed before remove_legacy existed keeps listing only', async () => {
    const res = await admin(env, 'POST', '/admin/bench', body());
    const { id, irm } = (await res.json()) as { id: string; irm: string };
    const key = await importAesKey(env.BENCH_KEY);
    if (!key) throw new Error('no key');
    const old = { code: 'ABCD-EFGH-JKLM-NPQR', steps: [{ setup: 'x-enroll', args: [], env: {} }], sql_databases: [] };
    env.DB.raw.prepare('UPDATE bench_bundles SET steps_enc = ? WHERE id = ?').run(await seal(key, JSON.stringify(old), id), id);
    const token = /\/bench\/([A-Za-z0-9_-]+) \| iex$/.exec(irm)?.[1] ?? '';
    const script = await (await app.request(`${BASE}/bench/${token}`, {}, env)).text();
    expect(script).toContain('$RemoveLegacy = $false');
  });

  it.each([
    ['a SQL part in args', { steps: [{ setup: 'x-enroll', args: ['{{SQL_PASSWORD}}'] }] }],
    ['an unknown SQL part', { steps: [{ setup: 'x-enroll', env: { A: '{{SQL_HOST}}' } }] }],
    ['remove_legacy not a boolean', { remove_legacy: 'yes' }],
  ])('rejects %s', async (_name, over) => {
    expect((await admin(env, 'POST', '/admin/bench', body(over))).status).toBe(400);
  });

  it('accepts SQL parts in env and says when a bundle uses them', () => {
    expect(usesSqlParts([{ ...EINVOICE_STEP }])).toBe(true);
    expect(usesSqlParts([{ setup: 'x', args: [], env: { A: '{{SQL:SMILE_FO}}' } }])).toBe(false);
  });

  it('never matches SQL Server services, even a named instance called like a product', () => {
    const sql = new RegExp(SQL_SERVICE_PATTERN, 'i');
    const bin = new RegExp(SQL_BINARY_PATTERN, 'i');
    for (const n of ['MSSQL$SMILEPMS', 'SQLAgent$SMILEPMS', 'MSSQLSERVER', 'SQLBrowser', 'SQLWriter', 'SQLTELEMETRY$SMILE']) expect(n).toMatch(sql);
    for (const n of ['SmilePmsAgent', 'MM.Membership.Agent', 'dl-agent']) expect(n).not.toMatch(sql);
    expect('"C:\\Program Files\\Microsoft SQL Server\\MSSQL15.SMILEPMS\\MSSQL\\Binn\\sqlservr.exe" -sSMILEPMS').toMatch(bin);
    expect('C:\\ProgramData\\MM-EInvoice\\mmei-agent.exe').not.toMatch(bin);
  });
});

const pwsh = ((): boolean => {
  try {
    execFileSync('pwsh', ['-NoProfile', '-Command', '1'], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
})();

/** Runs LEGACY_BODY in pwsh with fake services, tasks, sc.exe and schtasks.exe over a temp tree. */
function runLegacy(root: string, scFails: string[]): Record<string, unknown> {
  const p = (...parts: string[]): string => join(root, ...parts);
  const svc = (name: string, path: string, state = 'Running'): string =>
    `(New-Object PSObject -Property @{ Name = ${psQuote(name)}; DisplayName = ${psQuote(name + ' service')}; PathName = ${psQuote(path)}; State = ${psQuote(state)}; StartMode = 'Auto'; ProcessId = 0 })`;
  const script = [
    "$ErrorActionPreference = 'Stop'",
    `$env:ProgramData = ${psQuote(p('PD'))}`,
    `$env:ProgramW6432 = ${psQuote(p('PF'))}`,
    `$env:ProgramFiles = ${psQuote(p('PF'))}`,
    `[Environment]::SetEnvironmentVariable('ProgramFiles(x86)', ${psQuote(p('PF86'))})`,
    `$env:windir = ${psQuote(p('Win'))}`,
    `$env:SystemDrive = ${psQuote(root)}`,
    "$Product = 'dl-agent'",
    `$pf = ${psQuote(p('PF'))}`,
    `$Data = ${psQuote(p('PD', 'dl-agent'))}`,
    `$Exe = ${psQuote(p('PF', 'DevLibrary', 'dl-agent', 'dl-agent.exe'))}`,
    `$LegacyPattern = ${psQuote(LEGACY_SERVICE_PATTERN)}`,
    `$SqlServicePattern = ${psQuote(SQL_SERVICE_PATTERN)}`,
    `$SqlBinaryPattern = ${psQuote(SQL_BINARY_PATTERN)}`,
    '$St = @{ Legacy = @{} }',
    '$log = New-Object Collections.ArrayList',
    '$results = New-Object Collections.ArrayList',
    '$deleted = New-Object Collections.ArrayList',
    '$stopped = New-Object Collections.ArrayList',
    `$scFails = @(${scFails.map(psQuote).join(', ')})`,
    'function Say([string]$m) { [void]$log.Add($m) }',
    'function Warn([string]$m) { [void]$log.Add($m) }',
    'function Fail([string]$m) { [void]$log.Add($m) }',
    'function Out-Line([string]$m, [string]$c) { [void]$log.Add($m) }',
    'function Add-Result([string]$s, [string]$r, [string]$n) { [void]$results.Add($s + ' + "'|'" + ' + $r) }',
    LEGACY_BODY,
    // Fakes, defined after the body so they replace its Windows-only parts.
    'function Get-Services { @(',
    [
      svc('MMEI-Agent', p('PD', 'MM-EInvoice', 'mmei-agent.exe')),
      svc('MSSQL$SMILEPMS', `"${p('SQL', 'Microsoft SQL Server', 'Binn', 'sqlservr.exe')}" -sSMILEPMS`),
      svc('dl-agent', `"${p('PF', 'DevLibrary', 'dl-agent', 'dl-agent.exe')}" run`),
      svc('Spooler', p('Win', 'System32', 'spoolsv.exe')),
      svc('MM.Membership.Agent', `"${p('Apps', 'MM.Membership.Agent', 'MM.Membership.Agent.exe')}"`, 'Stopped'),
    ].join(',\n'),
    ') }',
    "function Get-LegacyTasks { @((New-Object PSObject -Property @{ Path = '\\'; Name = 'MM-Arrival Agent Watchdog'; Action = 'powershell.exe -File watchdog.ps1 -ServiceName MM.Arrival.Agent' })) }",
    'function Stop-LegacyService([string]$name) { [void]$stopped.Add($name); return "stopped" }',
    'function sc.exe { if ($scFails -contains $args[1]) { $global:LASTEXITCODE = 5 } else { [void]$deleted.Add($args[1]); $global:LASTEXITCODE = 0 } }',
    'function schtasks.exe { $global:LASTEXITCODE = 0 }',
    '$r = @(Remove-Legacy)',
    '$tasks = @(Get-LegacyTasks | Where-Object { Test-LegacyTask $_.Path ($_.Path + $_.Name + " " + $_.Action) })',
    '$own = Test-LegacyTask "\\" "\\dl-agent watchdog C:\\x\\dl-agent.exe watchdog --service MMEI"',
    '$ms = Test-LegacyTask "\\Microsoft\\Windows\\" "\\Microsoft\\Windows\\SmilePms thing"',
    "$dirs = @(Get-ServiceDir ('\"' + (Join-Path $Data 'x.exe') + '\"'), (Get-ServiceDir (Join-Path $env:windir 'System32\\svc.exe')), (Get-ServiceDir (Join-Path $pf 'one.exe')))",
    '@{ r = $r; count = $r.Count; deleted = @($deleted); stopped = @($stopped); results = @($results); tasks = $tasks.Count; own = $own; ms = $ms;',
    '   guarded = @($dirs | Where-Object { $_ }).Count; json = (Join-Path $Data "x") } | ConvertTo-Json -Compress -Depth 4',
  ].join('\n');
  const file = join(root, 'legacy.ps1');
  writeFileSync(file, script);
  return JSON.parse(execFileSync('pwsh', ['-NoProfile', '-File', file]).toString()) as Record<string, unknown>;
}

function tree(root: string): void {
  for (const d of [
    ['PD', 'MM-EInvoice'],
    ['PD', 'SmilePMS', 'Agent', 'state'],
    ['PD', 'SmilePMS', 'Other'],
    ['PD', 'dl-agent'],
    ['PF', 'MM-Arrival Agent', 'data'],
    ['PF', 'DevLibrary', 'dl-agent'],
    ['Apps', 'MM.Membership.Agent'],
    ['Win', 'System32'],
  ]) {
    mkdirSync(join(root, ...d), { recursive: true });
  }
  writeFileSync(join(root, 'PD', 'MM-EInvoice', 'config.enc'), 'sealed');
}

describe.skipIf(!pwsh)('bench script: Remove-Legacy (pwsh, fakes for the Windows parts)', () => {
  it('unregisters tasks, deletes matched services only, renames folders and records it all', () => {
    const root = mkdtempSync(join(tmpdir(), 'bench-legacy-'));
    try {
      tree(root);
      const out = runLegacy(root, []);
      expect(out.count).toBe(1);
      expect(out.r).toEqual([true]);
      expect(out.deleted).toEqual(['MM.Membership.Agent', 'MMEI-Agent']);
      expect(out.stopped).toEqual(['MM.Membership.Agent', 'MMEI-Agent']);
      expect(out.tasks).toBe(1);
      expect(out.own).toBe(false);
      expect(out.ms).toBe(false);
      expect(out.guarded).toBe(0);

      const renamed = (dir: string[]): string[] => readdirSync(join(root, ...dir)).filter((n) => /\.bench-old-\d{14}$/.test(n));
      expect(renamed(['PD'])).toHaveLength(1);
      expect(renamed(['PD', 'SmilePMS'])).toHaveLength(1);
      expect(renamed(['PF'])).toHaveLength(1);
      expect(renamed(['Apps'])).toHaveLength(1);
      // Untouched: dl-agent, a sibling folder, SQL Server.
      for (const keep of [['PD', 'dl-agent'], ['PD', 'SmilePMS', 'Other'], ['PF', 'DevLibrary', 'dl-agent']]) expect(existsSync(join(root, ...keep))).toBe(true);
      expect(existsSync(join(root, 'PD', 'MM-EInvoice'))).toBe(false);

      const files = readdirSync(join(root, 'PD', 'dl-agent')).filter((n) => /^bench-removed-\d{14}\.json$/.test(n));
      expect(files).toHaveLength(1);
      const rec = JSON.parse(readFileSync(join(root, 'PD', 'dl-agent', files[0] ?? ''), 'utf8')) as {
        services: { name: string; path: string }[];
        tasks: { name: string }[];
        dirs: { from: string; to: string }[];
      };
      expect(rec.services.map((s) => s.name)).toEqual(['MM.Membership.Agent', 'MMEI-Agent']);
      expect(rec.tasks.map((t) => t.name)).toEqual(['MM-Arrival Agent Watchdog']);
      expect(rec.dirs).toHaveLength(4);
      for (const d of rec.dirs) expect(d.to).toBe(`${d.from}${d.to.slice(d.from.length)}`);
      expect(readFileSync(join(rec.dirs.find((d) => d.from.endsWith('MM-EInvoice'))?.to ?? '', 'config.enc'), 'utf8')).toBe('sealed');
      expect((out.results as string[]).filter((r) => r.endsWith('|ok'))).toHaveLength(7);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it('stops at the first failure, renames nothing after it and records what was done', () => {
    const root = mkdtempSync(join(tmpdir(), 'bench-legacy-'));
    try {
      tree(root);
      const out = runLegacy(root, ['MMEI-Agent']);
      expect(out.r).toEqual([false]);
      expect(out.deleted).toEqual(['MM.Membership.Agent']);
      expect(out.results).toContain('remove service MMEI-Agent|FAILED');
      expect(existsSync(join(root, 'PD', 'MM-EInvoice', 'config.enc'))).toBe(true);
      const files = readdirSync(join(root, 'PD', 'dl-agent')).filter((n) => n.startsWith('bench-removed-'));
      const rec = JSON.parse(readFileSync(join(root, 'PD', 'dl-agent', files[0] ?? ''), 'utf8')) as { services: unknown[]; dirs: unknown[] };
      expect(rec.services).toHaveLength(1);
      expect(rec.dirs).toHaveLength(0);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it('the full script with remove_legacy on parses clean and resolves the SQL parts', () => {
    const script = benchScript({
      hub: BASE,
      label: 'bench',
      tenant_id: 'bench-demo',
      site_id: 'test-pc',
      expires_at: T0,
      code: 'ABCD-EFGH-JKLM-NPQR',
      steps: [EINVOICE_STEP],
      sql_databases: [],
      sql_fixups: [],
      sql_names: [],
      sql_parts: true,
      remove_legacy: true,
    });
    expect(script).not.toMatch(/`|\$\{/);
    const dir = mkdtempSync(join(tmpdir(), 'bench-ps-'));
    try {
      const file = join(dir, 'bench.ps1');
      writeFileSync(file, script);
      const parse = `$e = $null; $t = $null; [void][System.Management.Automation.Language.Parser]::ParseFile('${file}', [ref]$t, [ref]$e); $e.Count`;
      expect(execFileSync('pwsh', ['-NoProfile', '-Command', parse]).toString().trim()).toBe('0');

      const resolve = /^function Resolve-Sql[\s\S]*?\n}\n/m.exec(script)?.[0] ?? '';
      expect(resolve).not.toBe('');
      const test = join(dir, 'resolve.ps1');
      writeFileSync(
        test,
        [
          '$SqlNames = @()',
          resolve,
          "$St = @{ Sql = 'Server=x;'; Parts = @{ SQL_SERVER = 'SRV\\SMILE'; SQL_USER = 'sa'; SQL_PASSWORD = 'p@ss'; SQL_AUTH = 'sql' } }",
          "$a = Resolve-Sql '{{SQL_SERVER}}|{{SQL_USER}}|{{SQL_PASSWORD}}|{{SQL_AUTH}}'",
          "$St.Parts = @{ SQL_SERVER = '.'; SQL_USER = ''; SQL_PASSWORD = ''; SQL_AUTH = 'windows' }",
          "$b = Resolve-Sql '{{SQL_SERVER}}|{{SQL_USER}}|{{SQL_PASSWORD}}|{{SQL_AUTH}}'",
          "$St.Sql = ''",
          "try { [void](Resolve-Sql '{{SQL_SERVER}}'); $c = 'no error' } catch { $c = $_.Exception.Message }",
          '@{ a = $a; b = $b; c = $c } | ConvertTo-Json -Compress',
        ].join('\n'),
      );
      expect(JSON.parse(execFileSync('pwsh', ['-NoProfile', '-File', test]).toString())).toEqual({
        a: 'SRV\\SMILE|sa|p@ss|sql',
        b: '.|||windows',
        c: 'it needs SQL Server for {{SQL_SERVER}} and none is connected',
      });
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
