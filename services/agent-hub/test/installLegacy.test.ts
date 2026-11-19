import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { legacyInstallScript } from '../src/install/legacyScript';
import { ARRIVAL_LEGACY_SQL_BODY } from '../src/install/arrival';
import { arrivalSqlOf, installScript, legacyScript } from '../src/install/script';
import { setNow, T0, testEnv, type TestEnv } from './helpers';

/** The previous installer, served at /install/legacy.ps1 for builds without `dl-agent install`. */
const BASE = 'https://hub.test';
let env: TestEnv;

const get = async (path: string): Promise<Response> => app.request(`${BASE}${path}`, {}, env);

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
});
afterEach(() => vi.useRealTimers());

describe('legacy install script', () => {
  it('is served with this hub as default and the same arrival body as /install.ps1', async () => {
    const res = await get('/install/legacy.ps1');
    expect(res.status).toBe(200);
    expect(res.headers.get('Cache-Control')).toContain('no-store');
    const text = await res.text();
    expect(text).toContain(`[string]$Hub = '${BASE}'`);
    expect(text).toContain('# DevLibrary Agent installer.');
    expect(text).not.toMatch(/__HUBS?__|__ARRIVAL_SQL__/);
    expect(text).toContain(ARRIVAL_LEGACY_SQL_BODY);
    // Whatever body the arrival slice makes the default of installScript reaches the legacy script.
    const real = "function Get-ArrivalLegacySql {\n  $St.SqlFrom = 'old'\n  return 'Server=.;Integrated Security=True;'\n}";
    expect(arrivalSqlOf(installScript(BASE, [], real))).toBe(real);
    expect(arrivalSqlOf(installScript(BASE))).toBe(ARRIVAL_LEGACY_SQL_BODY);
    expect(legacyScript(BASE, ['https://b.test'])).toContain("('https://b.test' -split ',')");
  });

  it('keeps the program files when a newer agent uninstalls itself before the download replaces it', async () => {
    const text = await (await get('/install/legacy.ps1')).text();
    expect(text).toContain("if ((& $Exe uninstall --help | Out-String) -match 'keep-files') { $argv += '--keep-files' }");
  });

  it('runs product setup and replaces the old agent only after enrolment, before the service install', async () => {
    const text = await (await get('/install/legacy.ps1')).text();
    expect(text).toContain("[string]$Setup = ''");
    expect(text).toContain('[string[]]$Replace = @()');
    const enroll = text.indexOf("@('enroll', '--hub'");
    const setup = text.indexOf('Invoke-Agent @($Setup, ');
    const replace = text.lastIndexOf('Remove-Replaced');
    const install = text.indexOf("Invoke-Agent @('install'");
    expect(enroll).toBeGreaterThan(0);
    expect(setup).toBeGreaterThan(enroll);
    expect(replace).toBeGreaterThan(setup);
    expect(install).toBeGreaterThan(replace);
    expect(text).toContain('Remove-Item Env:DL_AGENT_SETUP_ARGS');
  });

  it('asks Windows or SQL login, defaulting to what this machine saved, and tests before enrolling', async () => {
    const text = await (await get('/install/legacy.ps1')).text();
    expect(text).toContain("SQL login: [1] Windows  [2] SQL login");
    // The saved choice is the default (a reinstall with no change reuses it); else Windows.
    expect(text).toContain('& $Exe sql-setup --show --data $Data');
    expect(text).toContain("if ($saved.auth -eq 'sql') { $mode = '2' }");
    expect(text).toContain("$mode = '1'");
    expect(text).toContain("Read-Secret 'Password (Enter keeps the saved one)'");
    // Windows mode is tested as the service account (LocalSystem).
    expect(text).toContain("$argv += '--as-service'");
    // A failed test re-prompts.
    const loop = text.slice(text.indexOf('function Read-SqlLogin'), text.indexOf('function Set-SqlLoginUnattended'));
    expect(loop).toContain('while ($true)');
    expect(loop).toContain('Choose again');
    // The SQL step runs before enrolment so a failure never spends the code.
    const sql = text.indexOf('(Set-SqlLoginUnattended ([ref]$SqlPassword))');
    const enroll = text.indexOf("@('enroll', '--hub'");
    expect(sql).toBeGreaterThan(0);
    expect(enroll).toBeGreaterThan(sql);
    expect(text.indexOf('    Read-SqlLogin\n')).toBeLessThan(enroll);
  });

  it('takes the SQL login unattended without echoing the password and exits 1 on failure', async () => {
    const text = await (await get('/install/legacy.ps1')).text();
    expect(text).toContain("[ValidateSet('', 'windows', 'sql')][string]$SqlAuth = ''");
    expect(text).toContain('[string]$SqlUser = ');
    // The password is never a parameter: env DL_AGENT_SQL_PASSWORD or the hidden prompt only.
    const params = text.slice(text.indexOf('param('), text.indexOf('$ErrorActionPreference'));
    expect(params).not.toMatch(/SqlPassword|Password/);
    expect(text).not.toMatch(/-SqlPassword/);
    expect(text).toContain('    $SqlPassword = $env:DL_AGENT_SQL_PASSWORD\n');
    const unattended = text.slice(text.indexOf('function Set-SqlLoginUnattended([ref]$Password)'), text.indexOf('function Remove-OldCopies'));
    expect(unattended).toContain("} elseif (-not $pw -and $SqlAuth -eq 'sql' -and [Environment]::UserInteractive) {");
    expect(unattended).toContain("$pw = Read-Secret ('Password of SQL login ' + $user)");
    expect(unattended).toContain('$Password.Value = $pw');
    // The password reaches the agent only through the environment, removed right after.
    expect(text).toContain('$env:DL_AGENT_SQL_PASSWORD = $pw');
    expect(text).toContain('Remove-Item Env:DL_AGENT_SQL_PASSWORD');
    expect(text).not.toMatch(/--password/);
    for (const line of text.split('\n')) {
      if (/Say|Warn|Write-Host/.test(line)) expect(line).not.toMatch(/\$SqlPassword|\$pw\b/);
    }
    // A failure prints the structured result and exits 1; a new machine is not enrolled.
    expect(text).toContain("'[dl-agent] SQL_SETUP_RESULT '");
    expect(text).toContain("if ($SqlFailed -and -not $enrolled) { throw");
    expect(text).toContain('if ($SqlAuth) { exit 1 }');
    expect(text).toContain("FAILED: SQL login test (see above).' -ForegroundColor Red; exit 1");
  });

  it('offers the old Check-In agent SQL connection when this machine saved none (arrival hook)', async () => {
    const text = await (await get('/install/legacy.ps1')).text();
    expect(text).not.toContain('__ARRIVAL_SQL__');
    expect(text).toContain(ARRIVAL_LEGACY_SQL_BODY);
    // The arrival body needs $St, Add-Secret and Warn defined before it is spliced in.
    const body = text.indexOf(ARRIVAL_LEGACY_SQL_BODY);
    for (const before of ['$St = @{', 'function Add-Secret(', 'function Warn(']) {
      expect(text.indexOf(before)).toBeGreaterThan(0);
      expect(text.indexOf(before)).toBeLessThan(body);
    }
    const saved = text.slice(text.indexOf('function Get-SavedSql'), text.indexOf('function Get-LocalSqlServer'));
    expect(saved).toContain('if (-not $s.auth) { Add-LegacySql $s }');
    // Its password is offered in memory only: an empty answer keeps it, the source is named.
    expect(text).toContain('if (-not $pw -and $saved.password -and $user -eq $saved.user) { $pw = $saved.password }');
    expect(text).toContain("Say ($saved.from + ': '");
    // A caller can still splice another body in its place.
    const real = "function Get-ArrivalLegacySql { $St.SqlFrom = 'old'; return 'Server=.;Integrated Security=True;' }";
    const spliced = legacyInstallScript(BASE, [], real);
    expect(spliced).toContain(real);
    expect(spliced).not.toContain(ARRIVAL_LEGACY_SQL_BODY);
    expect(spliced.split('function Get-ArrivalLegacySql').length).toBe(2);
  });

  it('passes the SQL sign-in to arrival-enroll through env only and removes it after', async () => {
    const text = await (await get('/install/legacy.ps1')).text();
    const start = text.indexOf('# -- arrival sign-in');
    const end = text.indexOf('# -- end arrival sign-in');
    const block = text.slice(start, end);
    expect(start).toBeGreaterThan(text.indexOf('(Set-SqlLoginUnattended ([ref]$SqlPassword))'));
    expect(block).toContain("if ($Setup -eq 'arrival-enroll') {");
    expect(block).toContain('$env:DL_AGENT_ARRIVAL_SQL_AUTH = $SqlAuth');
    expect(block).toContain('$env:DL_AGENT_ARRIVAL_SQL_USER = $SqlUser');
    expect(block).toContain('$env:DL_AGENT_SQL_PASSWORD = $SqlPassword');
    expect(block).toContain('$SqlPassword = $null');
    for (const name of ['DL_AGENT_ARRIVAL_TOKEN', 'DL_AGENT_ARRIVAL_PMS', 'DL_AGENT_ARRIVAL_TENANT_KEY', 'DL_AGENT_ARRIVAL_SQL_AUTH', 'DL_AGENT_ARRIVAL_SQL_USER', 'DL_AGENT_SQL_PASSWORD']) {
      expect(block).toContain(`'${name}'`);
    }
    const run = text.indexOf("try { $ok = Invoke-Agent @($Setup, '--data', $Data) } finally {");
    expect(run).toBeGreaterThan(end);
    expect(text.slice(run, text.indexOf('Remove-Replaced', run))).toContain("foreach ($n in $SetupSecrets) { Remove-Item ('Env:' + $n)");
    // Never in argv: no agent call carries the password, the login or the mode.
    for (const line of text.split('\n').filter((l) => l.includes('Invoke-Agent @('))) {
      expect(line).not.toMatch(/\$SqlPassword|\$SqlUser|\$SqlAuth|\$pw\b/);
    }
    // Without arrival-enroll the password is dropped right after the SQL step.
    expect(text).toContain("if ($Setup -ne 'arrival-enroll') { $SqlPassword = $null }");
  });

  it('keeps the verified download until Move-Item installs it (RT-0164 test PC)', async () => {
    const text = await (await get('/install/legacy.ps1')).text();
    const start = text.indexOf('function Remove-OldCopies');
    const body = text.slice(start, text.indexOf('\nfunction ', start + 1));
    expect(start).toBeGreaterThan(0);
    expect(body).not.toContain(".download'");
    const download = text.indexOf("$tmp = $Exe + '.download'");
    const cleanup = text.indexOf('  Remove-OldCopies', download);
    const move = text.indexOf('Move-Item -Force $tmp $Exe');
    expect(cleanup).toBeGreaterThan(download);
    expect(move).toBeGreaterThan(cleanup);
  });

});
