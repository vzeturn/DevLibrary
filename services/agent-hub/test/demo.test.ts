import { execFileSync } from 'node:child_process';
import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { DEMO_MODULES, DEMO_MODULE_NAMES, demoModulesPs, demoVariables } from '../src/domain/demo';
import { DEMO_BLOCK } from '../src/install/demo';
import { installScript } from '../src/install/script';
import { admin, createTenant, enrollRequest, newKeys, setNow, T0, testEnv, type TestEnv } from './helpers';

const BASE = 'https://hub.test';
let env: TestEnv;

interface DemoSet {
  tenant_id: string;
  site_id: string;
  channel: string;
  enroll_code_id: string;
  enroll_code: string;
  expires_at: number;
  install_command: string;
  modules: { module: string; setup: string | null; source: string; secrets: { env: string; required: boolean }[]; options: { env: string; flag: string; value?: string }[] }[];
  operator_env: string[];
}

const body = (over: Record<string, unknown> = {}): Record<string, unknown> => ({ tenant_id: 'demo-bench', site_id: 'owner-pc', ...over });

async function create(over: Record<string, unknown> = {}): Promise<DemoSet> {
  const res = await admin(env, 'POST', '/admin/demo-bench', body(over));
  expect(res.status, await res.clone().text()).toBe(201);
  expect(res.headers.get('Cache-Control')).toContain('no-store');
  return (await res.json()) as DemoSet;
}

async function enroll(code: string): Promise<Response> {
  const { publicKey } = await newKeys();
  return enrollRequest(env, { code, public_key: publicKey, hostname: 'OWNER-PC', os: 'windows', arch: 'x86_64', version: '1.0.0', modules: [] });
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
});
afterEach(() => vi.useRealTimers());

describe('POST /admin/demo-bench', () => {
  it('needs the admin token', async () => {
    const res = await app.request(`${BASE}/admin/demo-bench`, { method: 'POST', body: JSON.stringify(body()) }, env);
    expect(res.status).toBe(401);
    expect(env.DB.raw.prepare('SELECT COUNT(*) AS n FROM enroll_codes').get()).toEqual({ n: 0 });
  });

  it('mints one one-time canary code for a new demo tenant with every module enabled', async () => {
    const set = await create();
    expect(set).toMatchObject({ tenant_id: 'demo-bench', site_id: 'owner-pc', channel: 'canary', expires_at: T0 + 24 * 3600 });
    expect(set.enroll_code).toMatch(/^[A-Z0-9-]{6,}$/);
    expect(env.DB.raw.prepare('SELECT demo FROM tenants WHERE id = ?').get('demo-bench')).toEqual({ demo: 1 });
    expect(set.install_command).toContain(`DownloadString('${BASE}/install.ps1'))) -Code '${set.enroll_code}' -Demo`);
    expect(set.install_command).not.toContain('-DemoModules');
    expect(set.modules.map((m) => m.module)).toEqual(DEMO_MODULE_NAMES);

    const first = await enroll(set.enroll_code);
    expect(first.status).toBe(200);
    const enrolled = (await first.json()) as { agent_id: string; channel: string; tenant: string };
    expect(enrolled).toMatchObject({ tenant: 'demo-bench', site: 'owner-pc', channel: 'canary' });
    const cfg = env.DB.raw.prepare('SELECT body FROM configs WHERE agent_id = ?').get(enrolled.agent_id) as { body: string };
    const modules = (JSON.parse(cfg.body) as { modules: Record<string, { enabled: boolean }> }).modules;
    expect(Object.keys(modules)).toEqual(DEMO_MODULE_NAMES);
    expect(Object.values(modules).every((m) => m.enabled)).toBe(true);
    // One time only.
    expect((await enroll(set.enroll_code)).status).not.toBe(200);
  });

  it('keeps only the hash of the code; the module codes never reach the hub', async () => {
    const set = await create({ settings: { DL_AGENT_DEMO_EINVOICE_URL: 'https://einvoice.example' } });
    const dump = JSON.stringify([
      env.DB.raw.prepare('SELECT * FROM enroll_codes').all(),
      env.DB.raw.prepare('SELECT * FROM admin_audit').all(),
    ]);
    expect(dump).not.toContain(set.enroll_code);
    // The answer names variables, never a value besides the code and the prefilled options.
    const text = JSON.stringify(set);
    expect(text.split(set.enroll_code).length).toBe(3); // enroll_code and install_command
    for (const m of set.modules) for (const s of m.secrets) expect(s).not.toHaveProperty('value');
    expect(set.modules.every((m) => m.source === (m.setup ? 'operator' : 'none'))).toBe(true);
  });

  it('lists what the operator sets: prefilled options, placeholders for every required code', async () => {
    const set = await create({ settings: { DL_AGENT_DEMO_EINVOICE_URL: 'https://einvoice.example', DL_AGENT_DEMO_HL_HOTEL_ID: 'H-42' } });
    expect(set.operator_env).toContain("$env:DL_AGENT_DEMO_EINVOICE_URL = 'https://einvoice.example'");
    expect(set.operator_env).toContain("$env:DL_AGENT_DEMO_HL_HOTEL_ID = 'H-42'");
    for (const m of DEMO_MODULES) {
      for (const s of m.secrets.filter((x) => x.required)) {
        expect(set.operator_env.some((l) => l.startsWith(`$env:${s.from} = '<`))).toBe(true);
      }
    }
    // Optional ones are listed in modules but not pushed on the operator.
    expect(set.operator_env.some((l) => l.includes('DL_AGENT_DEMO_ARRIVAL_PMS'))).toBe(false);
    expect(set.modules.find((m) => m.module === 'einvoice')?.options[0]).toMatchObject({ flag: '--platform-url', value: 'https://einvoice.example' });
  });

  it('takes a subset of modules', async () => {
    const set = await create({ modules: ['pda-sync', 'einvoice'] });
    expect(set.modules.map((m) => m.module)).toEqual(['einvoice', 'pda-sync']);
    expect(set.install_command).toMatch(/ -Demo -DemoModules 'pda-sync','einvoice'$/);
    const res = await enroll(set.enroll_code);
    const { agent_id } = (await res.json()) as { agent_id: string };
    const cfg = env.DB.raw.prepare('SELECT body FROM configs WHERE agent_id = ?').get(agent_id) as { body: string };
    expect(Object.keys((JSON.parse(cfg.body) as { modules: object }).modules)).toEqual(['einvoice', 'pda-sync']);
  });

  it('refuses a live tenant: its agents would move to canary', async () => {
    await createTenant(env, 'live-hotel', false);
    const res = await admin(env, 'POST', '/admin/demo-bench', body({ tenant_id: 'live-hotel' }));
    expect(res.status).toBe(409);
    expect(await res.json()).toMatchObject({ error: 'tenant_not_demo' });
    expect(env.DB.raw.prepare('SELECT COUNT(*) AS n FROM enroll_codes').get()).toEqual({ n: 0 });
  });

  it.each([
    ['a module code in settings', { settings: { DL_AGENT_DEMO_PDA_CODE: 'ABC-123' } }],
    ['an unknown variable', { settings: { PATH: 'x' } }],
    ['a URL that is not https', { settings: { DL_AGENT_DEMO_PMS_URL: 'http://pms.example' } }],
    ['a quote in a value', { settings: { DL_AGENT_DEMO_HL_HOTEL_ID: "a'b" } }],
    ['an unknown module', { modules: ['einvoice', 'nope'] }],
    ['a module twice', { modules: ['einvoice', 'einvoice'] }],
    ['no module', { modules: [] }],
    ['a bad tenant id', { tenant_id: 'a b' }],
    ['a lifetime over 72 h', { expires_hours: 73 }],
  ])('refuses %s', async (_name, over) => {
    const res = await admin(env, 'POST', '/admin/demo-bench', body(over));
    expect(res.status).toBe(400);
    expect(env.DB.raw.prepare('SELECT COUNT(*) AS n FROM enroll_codes').get()).toEqual({ n: 0 });
  });
});

/** All `.rs` text under a crate's `src`, to check the table against what each tool really reads. */
function crateSource(crate: string): string {
  const root = join(__dirname, '..', '..', '..', 'crates', crate, 'src');
  const out: string[] = [];
  const walk = (dir: string): void => {
    for (const name of readdirSync(dir)) {
      const p = join(dir, name);
      if (statSync(p).isDirectory()) walk(p);
      else if (name.endsWith('.rs')) out.push(readFileSync(p, 'utf8'));
    }
  };
  walk(root);
  return out.join('\n');
}

const CRATE: Record<string, string> = {
  einvoice: 'dl-agent-einvoice',
  'pms-site': 'dl-agent-pms',
  arrival: 'dl-agent-arrival',
  membership: 'dl-agent-membership',
  hotellink: 'dl-agent-hotellink',
  backup: 'dl-agent-backup',
  'pda-sync': 'dl-agent-pda-sync',
  'pms-device': 'dl-agent-pms-device',
  speaker: 'dl-agent-speaker',
};

describe('demo module table', () => {
  it('matches each module: its name, enrol tool, the variables it reads and its options', () => {
    for (const m of DEMO_MODULES) {
      const src = crateSource(CRATE[m.module] ?? '');
      expect(src, m.module).toContain(`pub const NAME: &str = "${m.module}"`);
      if (m.setup) expect(src, m.module).toContain(`"${m.setup}"`);
      for (const s of m.secrets) expect(src, `${m.module} ${s.to}`).toContain(`"${s.to}"`);
      for (const o of m.options) expect(src, `${m.module} ${o.flag}`).toContain(`Arg::option("${o.flag.slice(2)}")`);
      for (const f of m.flags) expect(src, `${m.module} ${f.flag}`).toContain(`Arg::flag("${f.flag.slice(2)}")`);
      if (m.reenroll) expect(m.flags.map((f) => f.flag), m.module).toContain(m.reenroll);
      for (const v of [...m.secrets.map((x) => x.from), ...m.options.map((x) => x.from)]) expect(v).toMatch(/^DL_AGENT_DEMO_[A-Z_]+$/);
    }
    expect(new Set(demoVariables()).size).toBe(demoVariables().length);
  });

  it('renders as PowerShell 2.0 hashtables, one per module, in table order', () => {
    const ps = demoModulesPs();
    expect(ps.startsWith('$DemoTable = @(\n')).toBe(true);
    expect(ps.match(/^ {2}@\{ Module = /gm)?.length).toBe(DEMO_MODULES.length);
    expect(ps).toContain(
      "@{ Module = 'pda-sync'; Setup = 'pda-enroll'; SecretFrom = @('DL_AGENT_DEMO_PDA_CODE', 'DL_AGENT_DEMO_PDA_SQL_ADMIN_PASSWORD', 'DL_AGENT_DEMO_PDA_SQL_DEFAULT_PASSWORD'); SecretTo = @('DL_AGENT_PDA_CODE', 'DL_AGENT_PDA_SQL_ADMIN_PASSWORD', 'DL_AGENT_PDA_SQL_DEFAULT_PASSWORD'); SecretNeed = @('DL_AGENT_DEMO_PDA_CODE'); OptionFrom = @('DL_AGENT_DEMO_PDA_URL', 'DL_AGENT_DEMO_PDA_SQL_ADMIN_USER'); OptionFlag = @('--url', '--sql-admin-user'); OptionNeed = @('DL_AGENT_DEMO_PDA_URL');",
    );
    // pms-device needs its API URL; with it unset the script skips just that module (`set DL_AGENT_DEMO_PMS_DEVICE_URL`).
    expect(ps).toContain("@{ Module = 'pms-device'; Setup = 'pms-device-enroll'; SecretFrom = @('DL_AGENT_DEMO_PMS_DEVICE_TOKEN'); SecretTo = @('DL_AGENT_PMS_DEVICE_TOKEN'); SecretNeed = @(); OptionFrom = @('DL_AGENT_DEMO_PMS_DEVICE_URL', 'DL_AGENT_DEMO_PMS_DEVICE_AGENT_ID'); OptionFlag = @('--api-url', '--agent-id'); OptionNeed = @('DL_AGENT_DEMO_PMS_DEVICE_URL');");
    expect(ps).toContain("@{ Module = 'speaker'; Setup = ''; SecretFrom = @(); SecretTo = @(); SecretNeed = @()");
  });
});

describe('install.ps1 -Demo', () => {
  const text = installScript(BASE);

  it('takes -Demo, -DemoEnv and -DemoModules, or env DL_AGENT_DEMO=1 with the short form', () => {
    const params = text.slice(text.indexOf('param('), text.indexOf('$DlState'));
    expect(params).toContain('[switch]$Demo,');
    expect(params).toContain('[hashtable]$DemoEnv = @{},');
    expect(params).toContain('[string[]]$DemoModules = @()\n)');
    expect(text).toContain("Demo = ([bool]$Demo -or ($env:DL_AGENT_DEMO -eq '1'))");
    expect(text).toContain(`& ([scriptblock]::Create((irm ${BASE}/install.ps1))) -Demo -Code XXXX-XXXX`);
    expect(text).not.toContain('__DEMO_');
    expect(text).toContain(demoModulesPs());
  });

  it('installs canary only and checks the module names before the code is spent', () => {
    const main = text.slice(text.indexOf('function Install-Agent'));
    const demo = main.indexOf('if ($DlState.Demo) {');
    expect(demo).toBeGreaterThan(0);
    expect(demo).toBeLessThan(main.indexOf('$info = Get-Info'));
    const check = main.slice(demo, main.indexOf('$v = [Environment]::OSVersion.Version'));
    expect(check).toContain("if ($Target -and $Target -ne 'canary') { Stop-Install 78");
    expect(check).toContain("if ($Channel -and $Channel -ne 'canary') { Stop-Install 78");
    expect(check).toContain("Stop-Install 78 ('Unknown demo module ' + $m + '.')");
    expect(check).toContain("$Target = 'canary'");
    expect(check).toContain("Stop-Install 78 '-DemoModules and -DemoEnv go with -Demo.'");
    expect(main).toContain("if ($DlState.Demo) { Stop-Install 78 ($Product + ' ' + $info['version'] + ' predates -Demo.')");
  });

  it('runs the demo after a finished install and always clears the variables', () => {
    const tail = text.slice(text.lastIndexOf('\nInstall-Agent\n'));
    expect(tail).toContain('try { if ($DlState.Done) { Invoke-Demo } }');
    expect(tail).toContain('finally { Clear-DemoEnv }');
    expect(text.indexOf('#<demo>')).toBeLessThan(text.lastIndexOf('\nInstall-Agent\n'));
  });

  it('hands module codes to the tools in env only, masks them and removes them', () => {
    const step = DEMO_BLOCK.slice(DEMO_BLOCK.indexOf('function Invoke-DemoStep'), DEMO_BLOCK.indexOf('function Restart-DemoAgent'));
    // argv holds the setup, the option flags and values and --data: nothing from SecretFrom.
    const argvLines = step.split('\n').filter((l) => l.includes('$argv +=') || l.includes('$argv = '));
    for (const l of argvLines) expect(l).not.toMatch(/sFrom|SecretFrom|sTo/);
    expect(step).toContain("[Environment]::SetEnvironmentVariable([string]$sTo[$i], $v, 'Process')");
    expect(step).toContain('$DemoState.Secrets += $v');
    expect(step).toContain("foreach ($n in $set) { [Environment]::SetEnvironmentVariable($n, $null, 'Process') }");
    expect(step).toContain("Write-Host ('    ' + (Hide-Demo ([string]$_)))");
    expect(step).toContain("return @('skip', ('set ' + ($missing -join ', ')))");
    const clear = DEMO_BLOCK.slice(DEMO_BLOCK.indexOf('function Clear-DemoEnv'));
    expect(clear).toContain('@($row.SecretFrom) + @($row.SecretTo) + @($row.OptionFrom)');
    expect(clear).toContain('$DemoEnv.Clear()');
    // Nothing is written to disk by the demo block.
    expect(DEMO_BLOCK).not.toMatch(/Set-Content|Add-Content|Out-File|WriteAllText|>\s*\$/);
  });

  it('runs the self test and prints one pass/warn/fail row per module', () => {
    expect(DEMO_BLOCK).toContain('$lines = @(& $exe selftest --trigger install --data $data 2>&1');
    expect(DEMO_BLOCK).toContain("'^\\s*(pass|warn|fail|skip)\\s+(\\S+)'");
    expect(DEMO_BLOCK).toContain('Format-Table -Property Module, Enrol, Selftest, Note -AutoSize');
    expect(DEMO_BLOCK).toContain("$_.Enrol -eq 'FAILED' -or $_.Selftest -eq 'fail'");
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

describe.skipIf(!pwsh)('install.ps1 -Demo in pwsh', () => {
  const run = (script: string): string => {
    const dir = mkdtempSync(join(tmpdir(), 'demo-ps-'));
    try {
      const file = join(dir, 't.ps1');
      writeFileSync(file, script);
      return execFileSync('pwsh', ['-NoProfile', '-File', file]).toString().trim();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  };

  it('parses clean with the demo block', () => {
    const dir = mkdtempSync(join(tmpdir(), 'demo-parse-'));
    try {
      const file = join(dir, 'install.ps1');
      writeFileSync(file, installScript(BASE));
      const parse = `$e = $null; $t = $null; [void][System.Management.Automation.Language.Parser]::ParseFile('${file}', [ref]$t, [ref]$e); $e.Count`;
      expect(execFileSync('pwsh', ['-NoProfile', '-Command', parse]).toString().trim()).toBe('0');
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  /** The demo block with stand-ins for what install.ps1 defines around it. */
  const harness = (demoModules: string, demoEnv: string, body: string): string =>
    [
      'function Say([string]$m) { }',
      'function Warn([string]$m) { }',
      `$DemoModules = ${demoModules}`,
      `$DemoEnv = ${demoEnv}`,
      DEMO_BLOCK.split('__DEMO_MODULES__').join(demoModulesPs()),
      body,
    ].join('\n');

  it('takes the worst self-test status per module', () => {
    const lines = ['STATUS  MODULE    CHECK  DETAIL', 'pass    core      hub    ok', 'warn    einvoice  sql    slow', 'pass    einvoice  token  ok', 'fail    pda-sync  id     none', '        remedy: run pda-enroll', '', '2 pass, 1 warn, 1 fail, 0 skip'];
    const out = run(harness('@()', '@{}', `$s = Get-DemoStatus @(${lines.map((l) => `'${l}'`).join(', ')}); ($s.Keys | Sort-Object | ForEach-Object { $_ + '=' + $s[$_] }) -join ';'`));
    expect(out).toBe('core=pass;einvoice=warn;pda-sync=fail');
  });

  it('runs a tool with the code in env only, then removes it; skips a module without its code', () => {
    const dir = mkdtempSync(join(tmpdir(), 'demo-tool-'));
    try {
      // A stand-in for dl-agent: prints its argv and the variable the PDA tool reads.
      const fake = join(dir, 'fake.ps1');
      writeFileSync(fake, "Write-Output ('argv=' + ($args -join ' ')); Write-Output ('code=' + $env:DL_AGENT_PDA_CODE)\n$global:LASTEXITCODE = 0\n");
      const out = run(
        harness(
          "@('pda-sync', 'einvoice')",
          "@{ DL_AGENT_DEMO_PDA_CODE = 'PDA-SECRET-1'; DL_AGENT_DEMO_PDA_URL = 'https://pda.example' }",
          [
            "$row = $DemoTable | Where-Object { $_.Module -eq 'pda-sync' }",
            `$r = Invoke-DemoStep $row '${fake}' 'C:\\data' 6>&1 | Out-String`,
            "$e = $DemoTable | Where-Object { $_.Module -eq 'einvoice' }",
            "$s = Invoke-DemoStep $e 'nope' 'C:\\data'",
            "$rows = (@(Get-DemoRows) | ForEach-Object { $_.Module }) -join ','",
            "Write-Output ($r.Trim() + '|after=' + [string]$env:DL_AGENT_PDA_CODE + '|' + ($s -join ':') + '|rows=' + $rows)",
          ].join('\n'),
        ),
      );
      expect(out).toContain('argv=pda-enroll --url https://pda.example --data C:\\data');
      expect(out).toContain('code=***');
      expect(out).not.toContain('PDA-SECRET-1');
      expect(out).toContain('|after=|');
      expect(out).toContain('skip:set DL_AGENT_DEMO_EINVOICE_TOKEN, DL_AGENT_DEMO_EINVOICE_URL');
      expect(out).toContain('|rows=einvoice,pda-sync');
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
