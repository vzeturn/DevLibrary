/**
 * The bench script served at `/bench/<token>` (RT-0164 BENCH): one `irm … | iex` on a test PC
 * lists the older per-product agents (never touching them), applies the SQL fixups to the restored
 * copy, installs dl-agent on canary for a demo tenant (no `-Replace`), registers the SQL databases,
 * runs each product's setup step and the self test. Windows PowerShell 5.1 (parses on 2.0: no JSON cmdlets, no
 * `::new()`, no `[ordered]`). Like `script.ts`, the body is free of backticks and dollar-brace so it
 * lives in a raw template literal; the generated data block replaces `__DATA__`.
 *
 * Secrets (enrolment code, env values, SQL password) are only in memory: never printed, never on a
 * command line, removed from the environment after each step; any agent output line that contains
 * one is masked before it reaches the console or the log.
 */
import type { BenchStep, SqlFixup } from '../domain/bench';
import { toBase64 } from '../lib/crypto';

/**
 * Windows services of the per-product agents dl-agent takes over (E-Invoice, SmilePmsAgent,
 * SmileHLAgent, MM.Arrival.Agent, MM.Membership.Agent, SRBackup, PdaSyncAgent), matched
 * case-insensitively against name, display name and path. The bench only lists them.
 */
export const LEGACY_SERVICE_PATTERN =
  'mmei|e-?invoice|smile ?pms|smile ?hl|mm\\.arrival|mm\\.membership|arrival\\.agent|membership\\.agent|srbackup|pda ?sync';

const BODY = String.raw`# DevLibrary bench bundle: lists the older per-product agents (they keep running: nothing here stops,
# removes or replaces them), applies the SQL fixups, installs dl-agent (canary), registers the SQL
# databases, runs each product setup step, then the self test. Run in Windows PowerShell as Administrator:
#   irm <link> | iex
# To pass a SQL Server connection instead of answering the prompt (it then sits in the console history):
#   & ([scriptblock]::Create((irm <link>))) -SqlConnection 'Server=.;User Id=sa;Password=...'
# The link carries an enrolment code, works a limited number of times and expires: do not share it.
param([string]$SqlConnection = '')
& {
param([string]$SqlConnection)
$ErrorActionPreference = 'Stop'
__DATA__
$Product = 'dl-agent'
$pf = $env:ProgramW6432
if (-not $pf) { $pf = $env:ProgramFiles }
$Exe = Join-Path $pf ('DevLibrary\' + $Product + '\' + $Product + '.exe')
$Data = Join-Path $env:ProgramData $Product
# Steps point their own install/config/state directories here, away from the older agents' ones.
$BenchDir = Join-Path $Data 'bench'
$St = @{ Log = ''; Sql = ''; Exit = 0; Legacy = @{}; Secrets = (New-Object Collections.ArrayList); Results = (New-Object Collections.ArrayList) }
[void]$St.Secrets.Add($Code)

function Hide([string]$m) {
  foreach ($s in $St.Secrets) { if ($s -and $s.Length -ge 4) { $m = $m.Replace($s, '***') } }
  return $m
}
function Out-Line([string]$m, [string]$color) {
  $m = Hide $m
  if ($color) { Write-Host $m -ForegroundColor $color } else { Write-Host $m }
  if ($St.Log) {
    try { Add-Content -Path $St.Log -Value $m -Encoding UTF8 }
    catch { $St.Log = ''; Write-Host ('[bench] Log file disabled: ' + $_.Exception.Message) -ForegroundColor Yellow }
  }
}
function Say([string]$m) { Out-Line ('[bench] ' + $m) '' }
function Warn([string]$m) { Out-Line ('[bench] ' + $m) 'Yellow' }
function Fail([string]$m) { Out-Line ('[bench] ' + $m) 'Red' }
function Good([string]$m) { Out-Line ('[bench] ' + $m) 'Green' }
function Add-Secret([string]$s) { if ($s) { [void]$St.Secrets.Add($s) } }
function Add-Result([string]$step, [string]$result, [string]$note) {
  [void]$St.Results.Add((New-Object PSObject -Property @{ Step = $step; Result = $result; Note = (Hide $note) }))
}
function UnixNow { [long]([DateTime]::UtcNow - (New-Object DateTime 1970, 1, 1, 0, 0, 0, ([DateTimeKind]::Utc))).TotalSeconds }

function Invoke-Agent([string[]]$argv) {
  $eap = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  $St.Exit = -1
  try {
    & $Exe @argv 2>&1 | ForEach-Object { Out-Line ('    ' + [string]$_) '' }
    $St.Exit = $LASTEXITCODE
  } catch {
    Out-Line ('    ' + $_.Exception.Message) 'Red'
  } finally {
    $ErrorActionPreference = $eap
  }
  return ($St.Exit -eq 0)
}

# Sets $vars for the agent process only (it inherits them), then restores what was there before.
function Invoke-WithEnv([hashtable]$vars, [string[]]$argv) {
  $saved = @{}
  try {
    foreach ($k in @($vars.Keys)) {
      $saved[$k] = [Environment]::GetEnvironmentVariable($k)
      Add-Secret $vars[$k]
      Set-Item -Path ('Env:' + $k) -Value $vars[$k]
    }
    return (Invoke-Agent $argv)
  } finally {
    foreach ($k in @($saved.Keys)) {
      if ($null -eq $saved[$k]) { Remove-Item -Path ('Env:' + $k) -ErrorAction SilentlyContinue }
      else { Set-Item -Path ('Env:' + $k) -Value $saved[$k] }
    }
  }
}

function Quote([string]$v) { return '"' + ($v -replace '"', '""') + '"' }
function Ask([string]$label, [string]$default) {
  $a = Read-Host ($label + ' [' + $default + ']')
  if ($a) { return $a.Trim() } else { return $default }
}
function Read-Secret([string]$label) {
  $sec = Read-Host $label -AsSecureString
  $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($sec)
  try { return [Runtime.InteropServices.Marshal]::PtrToStringAuto($bstr) } finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
}
function Get-SqlConn([string]$db) { return $St.Sql + 'Database=' + (Quote $db) + ';' }

# Connects to master with the server part of the connection, then looks up each database by name.
function Use-Sql([string]$base) {
  if (-not $base.EndsWith(';')) { $base += ';' }
  Add-Secret $base
  $c = $null
  try {
    $b = New-Object Data.SqlClient.SqlConnectionStringBuilder $base
    Add-Secret $b.Password
    $c = New-Object Data.SqlClient.SqlConnection ($base + 'Database=master;Connection Timeout=10;')
    $c.Open()
    Good ('SQL Server ' + $c.DataSource + ' answered (version ' + $c.ServerVersion + ').')
    foreach ($n in $SqlNames) {
      $cmd = $c.CreateCommand()
      $cmd.CommandText = 'SELECT DB_ID(@n)'
      [void]$cmd.Parameters.AddWithValue('@n', $n)
      $r = $cmd.ExecuteScalar()
      if ($null -eq $r -or $r -is [DBNull]) { Warn ('Database ' + $n + ' is not on this server: what uses it will fail.') }
      else { Say ('Database ' + $n + ' found.') }
    }
    if ($b.IntegratedSecurity) {
      Warn 'Windows authentication: the agent service runs as LocalSystem, so NT AUTHORITY\SYSTEM needs a login on this SQL Server (or use a SQL login).'
    }
    $St.Sql = $base
    return $true
  } catch {
    Fail ('SQL Server did not answer: ' + $_.Exception.Message)
    return $false
  } finally {
    if ($c) { $c.Dispose() }
  }
}

function Read-Sql {
  $d = @{ server = '.'; auth = '1'; user = 'sa' }
  while ($true) {
    $d.server = Ask 'SQL Server (. = this PC, HOST\INSTANCE or HOST,PORT)' $d.server
    $d.auth = Ask 'Authentication: 1 = SQL login, 2 = Windows' $d.auth
    $base = 'Server=' + (Quote $d.server) + ';'
    if ($d.auth -eq '2') {
      $base += 'Integrated Security=true;'
    } else {
      $d.user = Ask 'SQL login' $d.user
      $pw = Read-Secret 'Password'
      Add-Secret $pw
      $base += 'User Id=' + (Quote $d.user) + ';Password=' + (Quote $pw) + ';'
    }
    if (Use-Sql $base) { return }
    if ((Read-Host 'Try again? (Y/n)') -eq 'n') { Warn 'Going on without SQL Server: steps that need it will fail.'; return }
  }
}

# Read only: the older per-product agents are listed before anything changes and checked again at the end.
function Get-Legacy {
  if (Get-Command Get-CimInstance -ErrorAction SilentlyContinue) { $all = Get-CimInstance -ClassName Win32_Service -ErrorAction Stop }
  else { $all = Get-WmiObject -Class Win32_Service -ErrorAction Stop }
  @($all | Where-Object { $_.Name -ne $Product -and (($_.Name + ' ' + $_.DisplayName + ' ' + $_.PathName) -match $LegacyPattern) } | Sort-Object Name)
}
function Show-Legacy {
  try { $found = @(Get-Legacy) }
  catch { Warn ('Could not list the Windows services: ' + $_.Exception.Message); Add-Result 'older agents' 'warn' 'could not list services'; return }
  if ($found.Count -eq 0) { Say 'No older per-product agent service on this PC.'; Add-Result 'older agents' 'ok' 'none found'; return }
  Say ('Older per-product agents on this PC (' + $found.Count + '); the bench leaves them as they are:')
  foreach ($svc in $found) {
    $St.Legacy[[string]$svc.Name] = [string]$svc.State
    Out-Line ('    ' + $svc.Name + ' [' + $svc.State + '] ' + $svc.DisplayName + ' -> ' + $svc.PathName) ''
  }
  Add-Result 'older agents' 'ok' ([string]$found.Count + ' found, left untouched')
}
function Test-Legacy {
  if ($St.Legacy.Count -eq 0) { return }
  try { $after = @(Get-Legacy) } catch { Warn ('Could not list the Windows services again: ' + $_.Exception.Message); return }
  $changed = @()
  foreach ($k in @($St.Legacy.Keys)) {
    $state = 'missing'
    foreach ($svc in $after) { if ([string]$svc.Name -eq $k) { $state = [string]$svc.State } }
    if ($state -ne $St.Legacy[$k]) { $changed += ($k + ': ' + $St.Legacy[$k] + ' -> ' + $state) }
  }
  if ($changed.Count -gt 0) { Warn ('Older agents changed state during the bench: ' + ($changed -join '; ')); Add-Result 'older agents after' 'warn' ($changed -join '; ') }
  else { Say 'Older agents are in the same state as before the bench.'; Add-Result 'older agents after' 'ok' 'unchanged' }
}

# GO lines (sqlcmd style) split a script into batches; SqlCommand runs one batch at a time.
function Split-Batches([string]$sql) {
  @([regex]::Split($sql, '(?im)^[ \t]*GO[ \t]*;?[ \t]*\r?$') | Where-Object { $_.Trim() })
}
function Get-FixupSql($f) {
  if ($f.Sql64) { return [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($f.Sql64)) }
  Add-Secret $f.Url
  $wc = New-Object Net.WebClient
  $wc.Headers.Add('Cache-Control', 'no-cache')
  $bytes = $wc.DownloadData($f.Url)
  if ($f.Sha256) {
    $h = ([BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($bytes)) -replace '-', '').ToLower()
    if ($h -ne $f.Sha256) { throw 'the downloaded SQL does not match its sha256' }
  }
  $text = [Text.Encoding]::UTF8.GetString($bytes)
  if ($text.Length -gt 0 -and [int]$text[0] -eq 0xFEFF) { $text = $text.Substring(1) }
  return $text
}
# One fixup, one transaction: all its batches apply or none. Only its name and counts are printed.
function Invoke-Fixup($f) {
  $step = 'fixup ' + $f.Name + ' (' + $f.Database + ')'
  if (-not $St.Sql) { Fail ('SQL fixup ' + $f.Name + ': no SQL Server connection.'); Add-Result $step 'FAILED' 'no SQL Server connection'; return $false }
  $c = $null
  $tx = $null
  try {
    $batches = @(Split-Batches (Get-FixupSql $f))
    $c = New-Object Data.SqlClient.SqlConnection ((Get-SqlConn $f.Database) + 'Connection Timeout=15;')
    $c.Open()
    $tx = $c.BeginTransaction()
    $rows = 0
    foreach ($batch in $batches) {
      $cmd = $c.CreateCommand()
      $cmd.Transaction = $tx
      $cmd.CommandTimeout = 300
      $cmd.CommandText = $batch
      $n = $cmd.ExecuteNonQuery()
      if ($n -gt 0) { $rows += $n }
    }
    $tx.Commit()
    $tx = $null
    Good ('SQL fixup ' + $f.Name + ' on ' + $f.Database + ': applied (' + $batches.Count + ' batch(es), ' + $rows + ' row(s) changed).')
    Add-Result $step 'ok' ([string]$rows + ' row(s) changed')
    return $true
  } catch {
    $why = $_.Exception.Message
    if ($tx) { try { $tx.Rollback() } catch { Warn ('Rollback failed: ' + $_.Exception.Message) } }
    Fail ('SQL fixup ' + $f.Name + ' on ' + $f.Database + ' failed, nothing applied: ' + $why)
    Add-Result $step 'FAILED' $why
    return $false
  } finally {
    if ($c) { $c.Dispose() }
  }
}

function Resolve-Sql([string]$v) {
  foreach ($n in $SqlNames) {
    $ph = '{{SQL:' + $n + '}}'
    if ($v.Contains($ph)) {
      if (-not $St.Sql) { throw ('it needs SQL Server for ' + $n + ' and none is connected') }
      $v = $v.Replace($ph, (Get-SqlConn $n))
    }
  }
  return $v
}

function Invoke-SqlAdd([string]$name) {
  $step = 'sql-add ' + $name
  if (-not $St.Sql) { Add-Result $step 'FAILED' 'no SQL Server connection'; return }
  Say ('Registering database ' + $name)
  $ok = Invoke-WithEnv @{ DL_AGENT_SQL_CONNECTION = (Get-SqlConn $name) } @('sql-add', $name, '--data', $Data)
  if ($ok) { $ok = Invoke-Agent @('sql-test', $name, '--data', $Data) }
  if ($ok) { Add-Result $step 'ok' '' } else { Add-Result $step 'FAILED' ('exit code ' + $St.Exit) }
}

function Invoke-Step($s) {
  $name = [string]$s.Setup
  $argv = @()
  foreach ($a in $s.Argv) { $argv += ([string]$a).Replace('{{BENCH_DIR}}', $BenchDir) }
  Say ('Step: dl-agent ' + $name + ' ' + ($argv -join ' '))
  try {
    $vars = @{}
    foreach ($k in @($s.Env.Keys)) { $vars[$k] = Resolve-Sql (([string]$s.Env[$k]).Replace('{{BENCH_DIR}}', $BenchDir)) }
    if (Invoke-WithEnv $vars (@($name) + $argv + @('--data', $Data))) { Add-Result $name 'ok' '' }
    else { Fail ('Step ' + $name + ' failed (exit code ' + $St.Exit + '); going on with the next one.'); Add-Result $name 'FAILED' ('exit code ' + $St.Exit) }
  } catch {
    Fail ('Step ' + $name + ' failed: ' + $_.Exception.Message)
    Add-Result $name 'FAILED' $_.Exception.Message
  }
}

function Restart-Agent {
  Say 'Restarting the dl-agent service so the databases and module settings apply...'
  $beat = Join-Path $Data 'heartbeat.json'
  $since = (UnixNow) - 5
  try { Restart-Service -Name $Product -Force -ErrorAction Stop }
  catch { Fail ('Service restart failed: ' + $_.Exception.Message); Add-Result 'restart' 'FAILED' $_.Exception.Message; return }
  $deadline = (UnixNow) + 90
  while ((UnixNow) -lt $deadline) {
    if (Test-Path $beat) {
      $m = [regex]::Match([IO.File]::ReadAllText($beat), '"hub_at"\s*:\s*(\d+)')
      if ($m.Success -and ([long]$m.Groups[1].Value) -ge $since) { Good 'The agent is running and reached the hub.'; Add-Result 'restart' 'ok' ''; return }
    }
    Start-Sleep -Seconds 3
  }
  Warn 'No hub contact within 90 s of the restart; the self test runs anyway.'
  Add-Result 'restart' 'warn' 'no hub contact within 90 s'
}

function Invoke-Selftest {
  $eap = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try { $help = (& $Exe --help 2>&1 | Out-String) } finally { $ErrorActionPreference = $eap }
  if ($help -notmatch '\bselftest\b') {
    Warn 'This dl-agent build has no selftest command yet: self test skipped.'
    Add-Result 'selftest' 'skipped' 'not in this build'
    return
  }
  Say 'Self test (dl-agent selftest --trigger install); the agent uploads the result to the hub.'
  if (Invoke-Agent @('selftest', '--trigger', 'install', '--data', $Data)) { Add-Result 'selftest' 'ok' '' }
  else { Add-Result 'selftest' 'FAILED' ('exit code ' + $St.Exit + ', see the table above') }
}

function Skip-Rest([string]$why) {
  foreach ($n in $SqlDatabases) { Add-Result ('sql-add ' + $n) 'skipped' $why }
  foreach ($s in $Steps) { Add-Result ([string]$s.Setup) 'skipped' $why }
  Add-Result 'selftest' 'skipped' $why
}

$me = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  Write-Host '[bench] Run Windows PowerShell as Administrator (right-click > Run as administrator), then paste the line again.' -ForegroundColor Red
  return
}
try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072 }
catch { Write-Host '[bench] TLS 1.2 is not available: install .NET Framework 4.5 or newer if a download fails.' -ForegroundColor Yellow }

New-Item -ItemType Directory -Force -Path $Data | Out-Null
$St.Log = Join-Path $Data ('bench-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.log')
Say ('Bench ' + $Label + ': tenant ' + $Tenant + ', site ' + $Site + ', hub ' + $Hub + ', channel canary; link valid until ' + $Expires + '.')
Say ('PowerShell ' + $PSVersionTable.PSVersion + '; log ' + $St.Log + ' (secrets are never written to it).')

try {
  Show-Legacy
  if ($SqlNames.Count -gt 0) {
    if ($SqlConnection) {
      if (-not (Use-Sql $SqlConnection)) { Warn 'The -SqlConnection given did not connect; asking instead.'; Read-Sql }
    } else {
      Read-Sql
    }
  }

  $fixed = $true
  foreach ($f in $Fixups) { if (-not (Invoke-Fixup $f)) { $fixed = $false } }

  if (-not $fixed) {
    Fail 'A SQL fixup failed: dl-agent is not installed, so nothing runs against a copy that may still reach live services. Fix the cause and run the link again.'
    Add-Result 'install' 'skipped' 'a SQL fixup failed'
    Skip-Rest 'a SQL fixup failed'
  } else {
    # install.ps1 runs without its Replace option: the older agents stay installed and running next to dl-agent.
    Say 'Running the hub installer (install.ps1 -Channel canary)...'
    $wc = New-Object Net.WebClient
    $wc.Headers.Add('Cache-Control', 'no-cache')
    $wc.Encoding = [Text.Encoding]::UTF8
    & ([scriptblock]::Create($wc.DownloadString($Hub + '/install.ps1'))) -Code $Code -Channel canary
    $installed = (Test-Path $Exe) -and (Test-Path (Join-Path $Data 'identity.json')) -and [bool](Get-Service -Name $Product -ErrorAction SilentlyContinue)

    if ($installed) {
      Add-Result 'install' 'ok' ''
      New-Item -ItemType Directory -Force -Path $BenchDir | Out-Null
      foreach ($n in $SqlDatabases) { Invoke-SqlAdd $n }
      foreach ($s in $Steps) { Invoke-Step $s }
      Restart-Agent
      Invoke-Selftest
    } else {
      Fail 'dl-agent is not installed and enrolled (see the installer output above): the steps are skipped.'
      Add-Result 'install' 'FAILED' 'see the installer output'
      Skip-Rest 'not installed'
    }
  }
  Test-Legacy
} catch {
  Fail ('Bench stopped: ' + $_.Exception.Message)
  Add-Result 'bench' 'FAILED' $_.Exception.Message
}

Say 'Summary:'
$table = ($St.Results | Format-Table -Property Step, Result, Note -AutoSize | Out-String)
foreach ($line in ($table -split '\r?\n')) { if ($line.Trim()) { Out-Line $line '' } }
$failed = @($St.Results | Where-Object { $_.Result -eq 'FAILED' }).Count
if ($failed -eq 0) { Good 'Bench finished: every step passed.' } else { Fail ('Bench finished with ' + $failed + ' failed step(s); the log is ' + $St.Log) }
} $SqlConnection
`;

/** A PowerShell single-quoted literal: quote characters (ASCII and the typographic ones PowerShell also honours) are doubled. */
export function psQuote(value: string): string {
  return `'${value.replace(/['‘’‚‛]/g, (q) => q + q)}'`;
}

function psArray(items: readonly string[]): string {
  return items.length ? `@(${items.map(psQuote).join(', ')})` : '@()';
}

function psStep(s: BenchStep): string {
  const env = Object.entries(s.env)
    .map(([k, v]) => `${psQuote(k)} = ${psQuote(v)}`)
    .join('; ');
  return `  @{ Setup = ${psQuote(s.setup)}; Argv = ${psArray(s.args)}; Env = @{ ${env} } }`;
}

function psFixup(f: SqlFixup, i: number): string {
  const sql64 = f.sql_inline === undefined ? '' : toBase64(new TextEncoder().encode(f.sql_inline));
  return [
    `  @{ Name = ${psQuote(f.name ?? `fixup ${i + 1}`)}; Database = ${psQuote(f.database)}`,
    `Url = ${psQuote(f.sql_url ?? '')}; Sha256 = ${psQuote(f.sha256 ?? '')}; Sql64 = ${psQuote(sql64)} }`,
  ].join('; ');
}

export interface BenchScriptInput {
  hub: string;
  label: string;
  tenant_id: string;
  site_id: string;
  expires_at: number;
  code: string;
  steps: BenchStep[];
  sql_databases: string[];
  sql_fixups: SqlFixup[];
  /** Registered and fixed-up databases plus those named by `{{SQL:…}}` placeholders. */
  sql_names: string[];
}

export function benchScript(b: BenchScriptInput): string {
  const expires = new Date(b.expires_at * 1000).toISOString().slice(0, 16).replace('T', ' ') + ' UTC';
  const data = [
    `$Hub = ${psQuote(b.hub)}`,
    `$Label = ${psQuote(b.label)}`,
    `$Tenant = ${psQuote(b.tenant_id)}`,
    `$Site = ${psQuote(b.site_id)}`,
    `$Expires = ${psQuote(expires)}`,
    `$Code = ${psQuote(b.code)}`,
    `$SqlDatabases = ${psArray(b.sql_databases)}`,
    `$SqlNames = ${psArray(b.sql_names)}`,
    `$Steps = @(\n${b.steps.map(psStep).join(',\n')}\n)`,
    `$Fixups = @(\n${b.sql_fixups.map(psFixup).join(',\n')}\n)`,
    `$LegacyPattern = ${psQuote(LEGACY_SERVICE_PATTERN)}`,
  ].join('\n');
  return BODY.split('__DATA__').join(data);
}
