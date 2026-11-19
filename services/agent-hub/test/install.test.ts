import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { admin, createTenant, manifestText, setNow, T0, testEnv, type TestEnv } from './helpers';

const BASE = 'https://hub.test';
let env: TestEnv;

const get = async (path: string): Promise<Response> => app.request(`${BASE}${path}`, {}, env);

/** A canary manifest whose windows-x86_64 build is stored on this hub. */
function hubManifest(version: string): string {
  return manifestText(version, 'canary', T0).replace('https://dl.test/a.exe', `${BASE}/v1/artifacts/dl-agent/${version}/windows-x86_64/dl-agent.exe`);
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
});
afterEach(() => vi.useRealTimers());

describe('install script', () => {
  it('serves the installer with this hub as default, never cached', async () => {
    const res = await get('/install.ps1');
    expect(res.status).toBe(200);
    expect(res.headers.get('Cache-Control')).toContain('no-store');
    const text = await res.text();
    expect(text).toContain(`[string]$Hub = '${BASE}'`);
    expect(text).not.toContain('__HUB__');
  });

  it('runs product setup and replaces the old agent only after enrolment, before the service install', async () => {
    const text = await (await get('/install.ps1')).text();
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
    const text = await (await get('/install.ps1')).text();
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
    const sql = text.indexOf('Set-SqlLoginUnattended)');
    const enroll = text.indexOf("@('enroll', '--hub'");
    expect(sql).toBeGreaterThan(0);
    expect(enroll).toBeGreaterThan(sql);
    expect(text.indexOf('    Read-SqlLogin\n')).toBeLessThan(enroll);
  });

  it('takes the SQL login unattended without echoing the password and exits 1 on failure', async () => {
    const text = await (await get('/install.ps1')).text();
    expect(text).toContain("[ValidateSet('', 'windows', 'sql')][string]$SqlAuth = ''");
    expect(text).toContain('[string]$SqlUser = ');
    expect(text).toContain('[string]$SqlPassword = $env:DL_AGENT_SQL_PASSWORD');
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

  it('keeps the verified download until Move-Item installs it (RT-0164 test PC)', async () => {
    const text = await (await get('/install.ps1')).text();
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

  it('enrolment codes come with the one-line install command', async () => {
    await createTenant(env, 'demo', true);
    const res = await admin(env, 'POST', '/admin/enroll-codes', { tenant_id: 'demo', site_id: 'hq' });
    expect(res.status).toBe(201);
    const body = (await res.json()) as { code: string; install_command: string };
    expect(body.install_command).toContain(`DownloadString('${BASE}/install.ps1')`);
    expect(body.install_command).toContain(`-Code '${body.code}'`);
    expect(body.install_command).toContain('-bor 3072');
  });
});

describe('install info and download', () => {
  it('describes the latest build for a target as key=value lines with the hub clock', async () => {
    expect((await admin(env, 'PUT', '/admin/releases/dl-agent/1.2.0', manifestText('1.2.0', 'canary', T0))).status).toBe(200);
    const res = await get('/install/info/dl-agent/canary/windows-x86_64');
    expect(res.status).toBe(200);
    expect(res.headers.get('Cache-Control')).toContain('no-cache');
    const lines = Object.fromEntries((await res.text()).trim().split('\n').map((l) => l.split('=') as [string, string]));
    expect(lines).toMatchObject({
      version: '1.2.0',
      target: 'windows-x86_64',
      sha256: 'ab'.repeat(32),
      size: '123',
      server_time: String(T0),
      download: '/install/download/dl-agent/canary/1.2.0/windows-x86_64',
    });
  });

  it('names the available targets when the machine needs a build the release lacks', async () => {
    await admin(env, 'PUT', '/admin/releases/dl-agent/1.2.0', manifestText('1.2.0', 'canary', T0));
    const res = await get('/install/info/dl-agent/canary/windows7-x86');
    expect(res.status).toBe(404);
    expect(await res.json()).toMatchObject({ error: 'no_artifact', target: 'windows7-x86', available: ['windows-x86_64'] });
    expect((await get('/install/info/dl-agent/stable/windows-x86_64')).status).toBe(404);
    expect((await get('/install/info/dl-agent/beta/windows-x86_64')).status).toBe(400);
    expect((await get('/install/info/dl-agent/canary/Windows%20XP')).status).toBe(400);
  });

  it('streams a build stored on the hub without a signed agent', async () => {
    await admin(env, 'PUT', '/admin/releases/dl-agent/1.2.0', hubManifest('1.2.0'));
    env.ARTIFACTS.store.set('dl-agent/1.2.0/windows-x86_64/dl-agent.exe', new Uint8Array([1, 2, 3]));
    const res = await get('/install/download/dl-agent/canary/1.2.0/windows-x86_64');
    expect(res.status).toBe(200);
    expect(res.headers.get('Cache-Control')).toContain('no-store');
    expect(new Uint8Array(await res.arrayBuffer())).toEqual(new Uint8Array([1, 2, 3]));
    // The agent route stays signed-only.
    expect((await get('/v1/artifacts/dl-agent/1.2.0/windows-x86_64/dl-agent.exe')).status).toBe(401);
  });

  it('redirects builds hosted elsewhere and 404s unknown versions', async () => {
    await admin(env, 'PUT', '/admin/releases/dl-agent/1.2.0', manifestText('1.2.0', 'canary', T0));
    const res = await get('/install/download/dl-agent/canary/1.2.0/windows-x86_64');
    expect(res.status).toBe(302);
    expect(res.headers.get('Location')).toBe('https://dl.test/a.exe');
    expect((await get('/install/download/dl-agent/canary/9.9.9/windows-x86_64')).status).toBe(404);
  });
});
