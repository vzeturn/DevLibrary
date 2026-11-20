import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { ARRIVAL_LEGACY_SQL_BODY } from '../src/install/arrival';
import { installScript } from '../src/install/script';
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

  it('takes the short form with no options and the long forms with them', async () => {
    const text = await (await get('/install.ps1')).text();
    expect(text).toContain(`irm ${BASE}/install.ps1 | iex`);
    expect(text).toContain(`& ([scriptblock]::Create((irm ${BASE}/install.ps1))) -Code XXXX-XXXX -Channel canary`);
    expect(text).toContain(`DownloadString('${BASE}/install.ps1'))) -Code 'XXXX-XXXX'`);
    const params = text.slice(text.indexOf('param('), text.indexOf('$DlState'));
    // The target is the only positional option; everything else is named, unattended through env.
    expect(params.indexOf('[string]$Target = ')).toBe(params.indexOf('[string]$') );
    for (const p of [
      '[string]$Code = $env:DL_AGENT_ENROLL_CODE',
      '[string]$Channel = $env:DL_AGENT_CHANNEL',
      '[string]$SqlAuth = $env:DL_AGENT_SQL_AUTH',
      '[string]$SqlServer = $env:DL_AGENT_SQL_SERVER',
      '[string]$SqlUser = $env:DL_AGENT_SQL_USER',
      "[string]$Setup = ''",
      '[string[]]$Replace = @()',
    ]) {
      expect(params).toContain(p);
    }
    // Validated by hand (ValidateSet would reject an empty env default) and unknown arguments refused.
    expect(text).toContain("$Target -notmatch '^(|stable|latest|canary|auto|\\d+\\.\\d+\\.\\d+([-+][0-9A-Za-z.-]+)?)$'");
    expect(text).toContain("$Channel -notmatch '^(|stable|canary)$'");
    expect(text).toContain("$SqlAuth -notmatch '^(|windows|sql)$'");
    expect(text).toContain('if ($DlState.Extra.Count -gt 0) { Stop-Install 78');
  });

  it('runs on PowerShell 2.0 and fits a raw template: no backtick, no dollar-brace, ASCII only', async () => {
    for (const path of ['/install.ps1', '/install/legacy.ps1', '/uninstall.ps1']) {
      const text = await (await get(path)).text();
      expect(text, path).not.toContain('`');
      expect(text, path).not.toContain('${');
      // The previous installer keeps its text as it was (a few non-ASCII signs in comments).
      if (path !== '/install/legacy.ps1') expect(/^[\x09\x0a\x0d\x20-\x7e]*$/.test(text), path).toBe(true);
      for (const ps3 of ['Invoke-RestMethod', 'Invoke-WebRequest', 'ConvertFrom-Json', 'ConvertTo-Json', 'Get-FileHash', '::new(', '$PSScriptRoot', '[ordered]', '[pscustomobject]', ' -in ', ' -notin ']) {
        expect(text, `${path} ${ps3}`).not.toContain(ps3);
      }
    }
  });

  it('never takes, sets or prints a password: the binary asks for it hidden or reads env', async () => {
    const text = await (await get('/install.ps1')).text();
    const params = text.slice(text.indexOf('param('), text.indexOf('$DlState'));
    expect(params).not.toMatch(/password|secret/i);
    expect(text).not.toMatch(/-SqlPassword|--password|--sql-password|Read-Host/);
    expect(text).not.toMatch(/\$env:DL_AGENT_SQL_PASSWORD\s*=/);
    for (const line of text.split('\n')) {
      if (/Say|Warn|Write-Host/.test(line)) expect(line).not.toMatch(/\$legacy|\$Code\b|\$SqlConnection|assword/);
    }
    // The secrets this script hands over travel in env and are removed when it ends.
    expect(text).toContain('$env:DL_AGENT_ENROLL_CODE = $Code');
    expect(text).toContain('$env:DL_AGENT_SQL_CONNECTION = $SqlConnection');
    expect(text).toContain('$env:DL_AGENT_SQL_LEGACY = $legacy');
    const argv = text.slice(text.indexOf("$argv = @('install')"), text.indexOf('if ($Code) { $env:'));
    expect(argv).not.toMatch(/\$Code\b|\$SqlConnection|\$legacy\b/);
    const fin = text.slice(text.indexOf('  } finally {', text.indexOf('function Install-Agent')));
    expect(fin).toContain("foreach ($n in ($HandoffSecrets + $SetupSecrets)) { Remove-Item ('Env:' + $n)");
    for (const name of ['DL_AGENT_ENROLL_CODE', 'DL_AGENT_SQL_CONNECTION', 'DL_AGENT_SQL_LEGACY', 'DL_AGENT_SETUP_ARGS', 'DL_AGENT_SQL_PASSWORD', 'DL_AGENT_ARRIVAL_TOKEN']) {
      expect(text).toContain(`'${name}'`);
    }
  });

  it('rejects an HTML page or other garbage as an unreachable hub or a proxy', async () => {
    const text = await (await get('/install.ps1')).text();
    expect(text).toContain("$Unreachable = 'hub unreachable or blocked by a proxy'");
    const info = text.slice(text.indexOf('function Get-Info'), text.indexOf('function Show-AvBlock'));
    expect(info).toContain("$info['sha256'] -notmatch '^[0-9a-fA-F]{64}$'");
    expect(info).toContain('Stop-Install 69 ($Unreachable');
    expect(text).toContain("if ($manifest -notmatch '^\\s*\\{') { Stop-Install 69 ($Unreachable");
    // Every failure names the next step.
    for (const line of text.split('\n').filter((l) => l.includes('Stop-Install ') && !l.includes('function Stop-Install'))) {
      expect(line).toMatch(/Stop-Install \$?\w+ .+ '.+'\s*\}?\s*$|Stop-Install \$?\w+ .+\) \(.+\)\s*\}?\s*$/);
    }
    expect(text).toContain("Write-Host ('[dl-agent] Next: ' + $next) -ForegroundColor Yellow");
  });

  it('checks size, SHA-256 and the release signature before the build runs, then removes the download', async () => {
    const text = await (await get('/install.ps1')).text();
    const tmp = text.indexOf('$tmp = Join-Path $InstallDir');
    const size = text.indexOf("if ($size -ne [long]$info['size'])");
    const sha = text.indexOf("if ($sha -ne $info['sha256'].ToLower())");
    const modern = text.indexOf('if (-not (Test-Modern $tmp))');
    const manifest = text.indexOf("Get-HubText ('/install/manifest/' + $Product + '/' + $info['channel'] + '/' + $info['version'])");
    const verify = text.indexOf('& $verifier verify-self --manifest $mf --file $tmp --target $os --product $Product');
    const run = text.indexOf('& $tmp @argv');
    expect(tmp).toBeGreaterThan(0);
    const order = [tmp, size, sha, modern, manifest, verify, run];
    expect([...order].sort((x, y) => x - y)).toEqual(order);
    // An installed agent that knows verify-self is the verifier (trust carried over).
    expect(text).toContain('if (Test-Modern $Exe) { $verifier = $Exe }');
    expect(text.slice(verify)).toContain('if ($LASTEXITCODE -ne 0) { Stop-Install 65');
    const fin = text.slice(text.indexOf('  } finally {', text.indexOf('function Install-Agent')));
    expect(fin).toContain('foreach ($f in @($tmp, $mf))');
    expect(fin).toContain('Remove-Item -Force $f');
  });

  it('hands off to dl-agent install with the hub list, the target and the options', async () => {
    const text = await installScript(BASE, [BASE, 'https://b.test']);
    expect(text).toContain("foreach ($h in ('https://hub.test,https://b.test' -split ','))");
    const argv = text.slice(text.indexOf("$argv = @('install')"), text.indexOf('& $tmp @argv'));
    expect(argv).toContain('if ($Target) { $argv += $Target }');
    expect(argv).toContain("$argv += @('--hub', $hubs, '--data', $Data, '--link-timeout', [string]$LinkTimeout)");
    for (const o of ['--sql-name', '--sql-auth', '--sql-server', '--sql-user', '--sql-prompt', '--reenroll', '--no-defender', '--setup', '--replace']) {
      expect(argv).toContain(`'${o}'`);
    }
    // -Channel only picks the build: it is not handed over as the machine's channel.
    expect(argv).not.toContain('$Channel');
    expect(text).toContain('elseif (-not $lane) { $lane = Get-InstalledChannel $Data }');
    expect(text).toContain("if ($rc -ne 0) { Stop-Install $rc");
  });

  it('leaves Defender alone with another antivirus and never has the binary edit it (RT-0361)', async () => {
    const text = await (await get('/install.ps1')).text();
    // No download to %TEMP%: the build lands in the install folder.
    expect(text).not.toContain('GetTempPath');
    const trust = text.indexOf('    Set-AvTrust $InstallDir $Data');
    expect(trust).toBeGreaterThan(0);
    expect(trust).toBeLessThan(text.indexOf('$tmp = Join-Path $InstallDir'));
    const fn = text.slice(text.indexOf('function Set-AvTrust'), text.indexOf('function Get-Sha256'));
    // Another antivirus: warn with what to trust, return before any Defender cmdlet.
    const others = fn.indexOf('if ($others.Count -gt 0)');
    expect(others).toBeGreaterThan(0);
    expect(fn.slice(others, fn.indexOf('return', others))).toContain('Trusted application');
    expect(fn.indexOf('return', others)).toBeLessThan(fn.indexOf('Add-MpPreference'));
    expect(fn).toContain('Do not monitor application activity');
    // Defender only when it is active, in this console.
    expect(fn.indexOf('$mp.AntivirusEnabled')).toBeLessThan(fn.indexOf('Add-MpPreference -ExclusionPath'));
    const detect = text.slice(text.indexOf('$AvServices ='), text.indexOf('function Set-AvTrust'));
    expect(detect).toContain('avp|klnagent');
    expect(detect).toContain('Kaspersky');
    // The binary is always told not to touch Defender; so is the previous installer.
    const argv = text.slice(text.indexOf("$argv = @('install')"), text.indexOf('& $tmp @argv'));
    expect(argv).toContain("$argv += '--no-defender'");
    expect(argv).not.toContain('if ($NoDefender)');
    const legacy = text.slice(text.indexOf('function Invoke-Legacy'), text.indexOf('function Install-Agent'));
    expect(legacy).toContain('$p.NoDefender = $true');
  });

  it('hands a build without the installer verbs to the previous installer with the same options', async () => {
    const text = await (await get('/install.ps1')).text();
    const legacy = text.slice(text.indexOf('function Invoke-Legacy'), text.indexOf('function Install-Agent'));
    expect(legacy).toContain("Get-HubText '/install/legacy.ps1'");
    expect(legacy).toContain('Set-StrictMode -Off');
    expect(legacy).toContain('& ([scriptblock]::Create($text)) @p');
    for (const n of ['Code', 'SqlName', 'SqlConnection', 'SqlAuth', 'SqlServer', 'SqlUser', 'Setup', 'SqlPrompt', 'Reenroll', 'NoDefender', 'Replace', 'LinkTimeout']) {
      expect(legacy).toContain(n);
    }
    expect(text).toContain("if ($version) { Stop-Install 78 ($Product + ' ' + $version + ' predates version pinning.')");
  });

  it('offers the old Check-In agent SQL connection to the binary through env (arrival hook)', async () => {
    const text = await (await get('/install.ps1')).text();
    expect(text).not.toContain('__ARRIVAL_SQL__');
    expect(text).toContain(`#<arrival-sql>\n$St = @{ SqlFrom = ''; Secrets = @() }\nfunction Add-Secret([string]$s) { if ($s) { $St.Secrets += $s } }\n${ARRIVAL_LEGACY_SQL_BODY}\n#</arrival-sql>\n`);
    // The arrival slice's reader is the default body, spliced once.
    expect(text.split('function Get-ArrivalLegacySql').length).toBe(2);
    const body = text.indexOf(ARRIVAL_LEGACY_SQL_BODY);
    expect(text.indexOf('function Warn(')).toBeLessThan(body);
    expect(text).toContain('try { return [string](Get-ArrivalLegacySql) }');
    expect(text).toContain("if ($legacy) { $env:DL_AGENT_SQL_LEGACY = $legacy; $env:DL_AGENT_SQL_LEGACY_FROM = $St.SqlFrom }");
    const real = "function Get-ArrivalLegacySql { $St.SqlFrom = 'old'; return 'Server=.;Integrated Security=True;' }";
    const spliced = installScript(BASE, [], real);
    expect(spliced).toContain(real);
    expect(spliced).not.toContain(ARRIVAL_LEGACY_SQL_BODY);
    expect(spliced.split('function Get-ArrivalLegacySql').length).toBe(2);
  });

  it('exits with the code only when unattended; an interactive console stays open', async () => {
    const text = await (await get('/install.ps1')).text();
    expect(text).toContain('$global:LASTEXITCODE = $DlState.Exit');
    expect(text).toContain("$batch = (-not [Environment]::UserInteractive) -or [bool]$SqlAuth");
    expect(text).toContain('if ($DlState.Exit -ne 0 -and $batch) { exit $DlState.Exit }');
    expect(text.match(/\bexit \$/g)?.length).toBe(1);
    expect(text).toContain("Write-Host ($ok + ' Installation complete') -ForegroundColor Green");
    expect(text).toContain("Stop-Install 77 'Not running as Administrator.'");
    // Strict mode and preferences stay inside the installer (iex runs in the caller's scope).
    const main = text.slice(text.indexOf('function Install-Agent'));
    expect(main).toContain("Set-StrictMode -Version 2.0\n  $ErrorActionPreference = 'Stop'\n  $ProgressPreference = 'SilentlyContinue'");
    expect(text.slice(0, text.indexOf('function Install-Agent'))).not.toContain('$ErrorActionPreference');
  });

  it('serves the uninstaller: the agent removes itself, the data folder stays unless -Purge', async () => {
    const res = await get('/uninstall.ps1');
    expect(res.status).toBe(200);
    expect(res.headers.get('Cache-Control')).toContain('no-store');
    const text = await res.text();
    expect(text).toContain(`irm ${BASE}/uninstall.ps1 | iex`);
    expect(text).toContain('[switch]$Purge');
    expect(text).toContain("(& $exe uninstall --help | Out-String) -match 'keep-files'");
    expect(text).toContain("$argv = @('uninstall', '--data', $data)");
    expect(text).toContain("if ($Purge) { $argv += '--purge' }");
    // An older agent: what it leaves is removed here; the data folder only with -Purge.
    expect(text).toContain("& sc.exe delete $Product");
    expect(text).toContain("$task = $Display + ' Watchdog'");
    expect(text).toContain('if ($Purge -and (Test-Path $data)) { Remove-Item -Recurse -Force $data');
    expect(text.match(/\bexit \$/g)?.length).toBe(1);
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

  it('describes a pinned version on either channel and serves its signed manifest', async () => {
    const text = manifestText('1.2.0', 'canary', T0);
    await admin(env, 'PUT', '/admin/releases/dl-agent/1.2.0', text);
    const res = await get('/install/info/dl-agent/stable/windows-x86_64?version=1.2.0');
    expect(res.status).toBe(200);
    const lines = Object.fromEntries((await res.text()).trim().split('\n').map((l) => l.split('=') as [string, string]));
    expect(lines).toMatchObject({ version: '1.2.0', channel: 'canary', download: '/install/download/dl-agent/canary/1.2.0/windows-x86_64' });
    expect((await get('/install/info/dl-agent/stable/windows-x86_64?version=9.9.9')).status).toBe(404);
    expect((await get('/install/info/dl-agent/stable/windows-x86_64?version=latest')).status).toBe(400);
    const m = await get('/install/manifest/dl-agent/canary/1.2.0');
    expect(m.status).toBe(200);
    expect(m.headers.get('Cache-Control')).toContain('no-store');
    expect(await m.text()).toBe(text);
    expect((await get('/install/manifest/dl-agent/stable/1.2.0')).status).toBe(404);
    expect((await get('/install/manifest/dl-agent/canary/x.y')).status).toBe(400);
  });
});
