/**
 * The Windows installer served at `/install.ps1` (RT-0164): a thin bootstrap modelled on Claude
 * Code's install.ps1. It downloads the build, checks its size and SHA-256 against the hub, has
 * the release signature checked (`verify-self`), then hands off to the signed binary
 * (`dl-agent install`), which does the machine setup: old copies, service + watchdog, SQL login
 * (hidden password prompt), enrolment, `-Setup`, `-Replace`, link wait. The script itself checks
 * the antivirus (RT-0361): with a third-party one Defender is left alone and the operator is told
 * what to trust; with Defender in charge the exclusion is added here, never by a hidden child
 * PowerShell of the binary. The build is downloaded into the install folder, not %TEMP%.
 *
 * One script for every Windows: it parses and runs on PowerShell 2.0 (Windows 7 / Server 2008
 * R2: WebClient, no JSON cmdlets, no Get-FileHash, no Invoke-RestMethod) and is short enough for
 * `irm <hub>/install.ps1 | iex` on 5.1. A build that predates `dl-agent install` is installed by
 * the previous script (`/install/legacy.ps1`, `./legacyScript.ts`) with the same options.
 * Kept free of backticks and dollar-brace so it can live in a raw template literal; the hub
 * origin replaces `__HUB__`, the hub list `__HUBS__`.
 */
import { demoModulesPs } from '../domain/demo';
import { ARRIVAL_LEGACY_SQL_BODY } from './arrival';
import { DEMO_BLOCK } from './demo';
import { legacyInstallScript } from './legacyScript';

const SCRIPT = String.raw`# DevLibrary Agent installer. PowerShell as Administrator:
#   Windows 10 / Server 2016 and newer:   irm __HUB__/install.ps1 | iex
#   With options:   & ([scriptblock]::Create((irm __HUB__/install.ps1))) -Code XXXX-XXXX -Channel canary
#   Windows 7 / Server 2008 R2 (PowerShell 2.0):
#     [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072
#     & ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('__HUB__/install.ps1'))) -Code 'XXXX-XXXX'
# What is missing is asked: enrolment code, SQL login [1] Windows [2] SQL, server, user and the
# password (typed hidden; never a parameter, echoed or logged). Unattended: env DL_AGENT_ENROLL_CODE,
# DL_AGENT_CHANNEL, DL_AGENT_SQL_AUTH (windows|sql), DL_AGENT_SQL_SERVER, DL_AGENT_SQL_USER and
# DL_AGENT_SQL_PASSWORD. Running it again repairs or updates the agent and keeps its identity.
# Target (first argument): stable or canary makes this machine follow that channel; a version
# (1.4.2) holds it there until "dl-agent install stable"; auto drops both and follows the hub.
# -Channel only picks the build installed now (the hub keeps assigning the channel).
# Options: -SqlPrompt (SQL Server on another machine), -SqlName/-SqlConnection, -Reenroll (new
#   identity), -NoDefender, -LinkTimeout <seconds>, -Setup <sub-command> (its secrets through env
#   DL_AGENT_SETUP_ARGS, removed afterwards), -Replace <service name pattern> (an older per-product
#   agent: its services are stopped and deleted, its files and config stay).
# Demo bench (a test PC, canary, every product module on its demo tenant, then the self test):
#   & ([scriptblock]::Create((irm __HUB__/install.ps1))) -Demo -Code XXXX-XXXX
#   or env DL_AGENT_DEMO=1 (and DL_AGENT_ENROLL_CODE) with the short form. Module codes and URLs come
#   from env DL_AGENT_DEMO_* (or -DemoEnv @{ NAME = 'value' }), reach each enrol tool through the
#   variable it reads, never argv, and are cleared when the script ends; -DemoModules picks modules.
#   POST /admin/demo-bench on the hub answers the code and the list of variables.
# Remove: irm __HUB__/uninstall.ps1 | iex
param(
  [string]$Target = '',
  [string]$Code = $env:DL_AGENT_ENROLL_CODE,
  [string]$Hub = '__HUB__',
  [string]$Channel = $env:DL_AGENT_CHANNEL,
  [string]$Product = 'dl-agent',
  [string]$SqlName = '',
  [string]$SqlConnection = '',
  [switch]$SqlPrompt,
  [string]$SqlAuth = $env:DL_AGENT_SQL_AUTH,
  [string]$SqlServer = $env:DL_AGENT_SQL_SERVER,
  [string]$SqlUser = $env:DL_AGENT_SQL_USER,
  [switch]$Reenroll,
  [switch]$NoDefender,
  [int]$LinkTimeout = 120,
  [string]$Setup = '',
  [string[]]$Replace = @(),
  [switch]$Demo,
  [hashtable]$DemoEnv = @{},
  [string[]]$DemoModules = @()
)

$DlState = @{ Exit = 0; Extra = @($args); Done = $false; Demo = ([bool]$Demo -or ($env:DL_AGENT_DEMO -eq '1')) }
if ($DemoModules.Count -eq 0 -and $env:DL_AGENT_DEMO_MODULES) { $DemoModules = @(($env:DL_AGENT_DEMO_MODULES -split '[,\s]+') | Where-Object { $_ }) }
$MaxSkew = 240
# Secret env a -Setup sub-command reads (DL_AGENT_SETUP_ARGS too): removed when the installer ends.
$SetupSecrets = @('DL_AGENT_ARRIVAL_TOKEN', 'DL_AGENT_ARRIVAL_PMS', 'DL_AGENT_ARRIVAL_TENANT_KEY', 'DL_AGENT_ARRIVAL_SQL_AUTH', 'DL_AGENT_ARRIVAL_SQL_USER', 'DL_AGENT_SQL_PASSWORD', 'DL_AGENT_EINVOICE_TOKEN', 'DL_AGENT_PMS_CODE')
# Secret env this script hands to the binary: removed when the installer ends.
$HandoffSecrets = @('DL_AGENT_ENROLL_CODE', 'DL_AGENT_SQL_CONNECTION', 'DL_AGENT_SQL_LEGACY', 'DL_AGENT_SQL_LEGACY_FROM', 'DL_AGENT_SETUP_ARGS')

function Say([string]$m) { Write-Host ('[dl-agent] ' + $m) }
function Warn([string]$m) { Write-Host ('[dl-agent] ' + $m) -ForegroundColor Yellow }
function UnixNow { [long]([DateTime]::UtcNow - (New-Object DateTime 1970, 1, 1, 0, 0, 0, ([DateTimeKind]::Utc))).TotalSeconds }

# A failure with its exit code and the next step to take.
function Stop-Install([int]$exitCode, [string]$message, [string]$next) {
  $e = New-Object Exception $message
  $e.Data['code'] = $exitCode
  $e.Data['next'] = $next
  throw $e
}

# -- arrival legacy SQL (RT-0164): the old Check-In agent's SQL Server connection is the saved
#    answer when this machine saved none. Get-ArrivalLegacySql (the arrival slice's body from
#    src/install/arrival.ts by default) is spliced below; it needs $St,
#    Add-Secret and Warn. This script prints no transcript, so Add-Secret only keeps the list.
#<arrival-sql>
$St = @{ SqlFrom = ''; Secrets = @() }
function Add-Secret([string]$s) { if ($s) { $St.Secrets += $s } }
__ARRIVAL_SQL__
#</arrival-sql>
# The connection (password included) reaches the binary through env DL_AGENT_SQL_LEGACY only.
function Get-LegacySqlConnection {
  Set-StrictMode -Off
  try { return [string](Get-ArrivalLegacySql) }
  catch { Warn ('The old Check-In agent''s SQL connection was not read: ' + $_.Exception.Message); return '' }
}
# -- end arrival legacy SQL

function New-Client {
  $wc = New-Object Net.WebClient
  $wc.Headers.Add('Cache-Control', 'no-cache')
  $wc.Encoding = [Text.Encoding]::UTF8
  return $wc
}

function Get-WebError($err) {
  $ex = $err.Exception
  while ($ex -and -not ($ex -is [Net.WebException]) -and $ex.InnerException) { $ex = $ex.InnerException }
  return $ex
}

$Unreachable = 'hub unreachable or blocked by a proxy'
function Get-HubText([string]$path) {
  try { return (New-Client).DownloadString($Hub + $path) }
  catch {
    $ex = Get-WebError $_
    $body = ''
    if (($ex -is [Net.WebException]) -and $ex.Response) {
      $r = New-Object IO.StreamReader($ex.Response.GetResponseStream())
      $body = $r.ReadToEnd()
      $r.Close()
    }
    if ($body -match '^\s*\{\s*"error"') { Stop-Install 69 ('The hub refused ' + $path + ': ' + $body) 'Check -Channel or the version asked for; the hub admin can tell which builds are published.' }
    Stop-Install 69 ($Unreachable + ' (' + $Hub + $path + ': ' + $ex.Message + ')') ('Open ' + $Hub + '/healthz in a browser on this PC; allow it in the proxy or firewall, then run again.')
  }
}

function Get-Info([string]$lane, [string]$target, [string]$version) {
  $path = '/install/info/' + $Product + '/' + $lane + '/' + $target
  if ($version) { $path += '?version=' + $version }
  $info = @{}
  foreach ($line in ((Get-HubText $path) -split '\r?\n')) {
    $i = $line.IndexOf('=')
    if ($i -gt 0) { $info[$line.Substring(0, $i)] = $line.Substring($i + 1).Trim() }
  }
  # An HTML page (a proxy's login or block page) or anything else that is not the hub's answer.
  if (-not ($info.ContainsKey('sha256') -and $info.ContainsKey('download') -and $info.ContainsKey('server_time')) -or $info['sha256'] -notmatch '^[0-9a-fA-F]{64}$') {
    Stop-Install 69 ($Unreachable + ': ' + $Hub + ' answered something that is not the installer''s data') ('Open ' + $Hub + '/healthz in a browser on this PC; allow it in the proxy or firewall, then run again.')
  }
  return $info
}

# Called when a just-written file vanished: names the active antivirus and Defender's latest detection.
function Show-AvBlock {
  try {
    foreach ($a in @(Get-WmiObject -Namespace root\SecurityCenter2 -Class AntivirusProduct -ErrorAction Stop)) { Warn ('Antivirus on this PC: ' + $a.displayName) }
  } catch { Warn 'Antivirus list not available (Windows Server has no Security Center).' }
  if (Get-Command Get-MpThreatDetection -ErrorAction SilentlyContinue) {
    try {
      $d = @(Get-MpThreatDetection -ErrorAction Stop | Sort-Object InitialDetectionTime -Descending | Select-Object -First 1)
      if ($d.Count -gt 0) { Warn ('Windows Defender latest detection: id ' + $d[0].ThreatID + ' at ' + $d[0].InitialDetectionTime + ' on ' + ($d[0].Resources -join ', ')) }
    } catch { Warn ('Windows Defender detections not readable: ' + $_.Exception.Message) }
  }
}

# Another antivirus (RT-0361): Defender is passive then, and editing it from an unsigned program
# is what behaviour engines (Kaspersky PDM) flag as a trojan. Security Center names it on client
# Windows; Windows Server has none, so running services are matched too.
$AvServices = '^(avp|klnagent|ekrn|vsserv|bdredline|avast|avg|mcafee|mfemms|sophos|sepmasterservice|ntrtscan|bkav|csfalconservice)'
$AvDisplays = 'Kaspersky|ESET |Bitdefender|Avast|AVG Antivirus|McAfee|Sophos|Symantec|Norton|Trend Micro|Bkav|CMC Antivirus|Malwarebytes|SentinelOne'
function Get-OtherAv {
  $names = @()
  try {
    foreach ($a in @(Get-WmiObject -Namespace root\SecurityCenter2 -Class AntivirusProduct -ErrorAction Stop)) {
      if ($a.displayName -and ([string]$a.displayName) -notmatch 'Defender|Microsoft') { $names += [string]$a.displayName }
    }
  } catch { Write-Verbose ('Security Center not readable (Windows Server has none): ' + $_.Exception.Message) }
  foreach ($s in @(Get-Service -ErrorAction SilentlyContinue | Where-Object { $_.Status -eq 'Running' })) {
    if ($s.DisplayName -match 'Defender|Microsoft') { continue }
    if ($s.Name -match $AvServices -or $s.DisplayName -match $AvDisplays) { $names += [string]$s.DisplayName }
  }
  return @($names | Select-Object -Unique)
}

# With another antivirus: Defender is left alone and the operator is told what to trust. With
# Defender in charge: its exclusion is added here, in this visible console, before the download;
# the binary is always told --no-defender so it never starts a hidden PowerShell for it.
function Set-AvTrust([string]$installDir, [string]$data) {
  $others = @(Get-OtherAv)
  if ($others.Count -gt 0) {
    Warn ('Antivirus on this PC: ' + ($others -join ', ') + '. Windows Defender is left alone (it is passive).')
    Warn ('If it blocks the agent: add ' + $Exe + ' as a Trusted application and exclude ' + $installDir + ' and ' + $data + ' from scanning, then run the same command again.')
    if (($others -join ' ') -match 'Kaspersky') { Warn 'Kaspersky: Settings > Threats and exclusions > Specify trusted applications > Add, and choose "Do not monitor application activity".' }
    return
  }
  if ($NoDefender -or -not (Get-Command Add-MpPreference -ErrorAction SilentlyContinue)) { return }
  try {
    $mp = Get-MpComputerStatus -ErrorAction Stop
    if (-not $mp.AntivirusEnabled) { Say 'Windows Defender is not active: no exclusion added.'; return }
    Add-MpPreference -ExclusionPath @($installDir, $data) -ErrorAction Stop
    Say ('Windows Defender exclusion added for ' + $installDir + ' and ' + $data)
  } catch { Warn ('Windows Defender exclusion not added: ' + $_.Exception.Message) }
}

function Get-Sha256([string]$path) {
  $s = [IO.File]::OpenRead($path)
  try { $h = [Security.Cryptography.SHA256]::Create().ComputeHash($s) } finally { $s.Close() }
  return ([BitConverter]::ToString($h) -replace '-', '').ToLower()
}

# True when the binary has the installer verbs (help is printed on stdout, exit 0).
function Test-Modern([string]$exe) {
  if (-not (Test-Path $exe)) { return $false }
  return ((& $exe --help | Out-String) -match 'verify-self')
}

# The channel this machine follows now: its local choice, else its last heartbeat, else stable.
function Get-InstalledChannel([string]$data) {
  $choice = Join-Path $data 'update-channel'
  if (Test-Path $choice) {
    $c = ([IO.File]::ReadAllText($choice)).Trim()
    if ($c -eq 'stable' -or $c -eq 'canary') { return $c }
  }
  $beat = Join-Path $data 'heartbeat.json'
  if (Test-Path $beat) {
    $m = [regex]::Match([IO.File]::ReadAllText($beat), '"channel"\s*:\s*"(stable|canary)"')
    if ($m.Success) { return $m.Groups[1].Value }
  }
  return 'stable'
}

# The previous installer, for a build without the installer verbs; same options.
function Invoke-Legacy([string]$lane) {
  $p = @{ Hub = $Hub; Channel = $lane; Product = $Product; LinkTimeout = $LinkTimeout; Replace = $Replace }
  $named = @{ Code = $Code; SqlName = $SqlName; SqlConnection = $SqlConnection; SqlAuth = $SqlAuth; SqlServer = $SqlServer; SqlUser = $SqlUser; Setup = $Setup }
  foreach ($k in @($named.Keys)) { if ($named[$k]) { $p[$k] = $named[$k] } }
  if ($SqlPrompt) { $p.SqlPrompt = $true }
  if ($Reenroll) { $p.Reenroll = $true }
  # Set-AvTrust already handled Defender (RT-0361).
  $p.NoDefender = $true
  $text = Get-HubText '/install/legacy.ps1'
  if ($text -notmatch 'DevLibrary Agent installer') { Stop-Install 69 ($Unreachable + ': /install/legacy.ps1 is not the installer') ('Open ' + $Hub + '/healthz in a browser on this PC, then run again.') }
  Set-StrictMode -Off
  & ([scriptblock]::Create($text)) @p
}

function Install-Agent {
  Set-StrictMode -Version 2.0
  $ErrorActionPreference = 'Stop'
  $ProgressPreference = 'SilentlyContinue'
  $tmp = ''
  $mf = ''
  try {
    if ($DlState.Extra.Count -gt 0) { Stop-Install 78 ('Unknown argument: ' + ($DlState.Extra -join ' ')) 'Options are named (-Code, -Channel, -SqlAuth ...); the only positional one is the target: stable, canary or a version.' }
    if ($Target -notmatch '^(|stable|latest|canary|auto|\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?)$') { Stop-Install 78 ('Unknown target ' + $Target) 'Use stable, canary, auto or a version such as 1.4.2.' }
    if ($Channel -notmatch '^(|stable|canary)$') { Stop-Install 78 ('Unknown channel ' + $Channel) 'Use -Channel stable or -Channel canary.' }
    if ($SqlAuth -notmatch '^(|windows|sql)$') { Stop-Install 78 ('Unknown SQL login mode ' + $SqlAuth) 'Use -SqlAuth windows or -SqlAuth sql.' }
    if ($DlState.Demo) {
      if ($Target -and $Target -ne 'canary') { Stop-Install 78 ('-Demo installs the canary build, not ' + $Target + '.') 'Leave the target out (or use canary) with -Demo.' }
      if ($Channel -and $Channel -ne 'canary') { Stop-Install 78 ('-Demo installs the canary build, not -Channel ' + $Channel + '.') 'Leave -Channel out with -Demo.' }
      foreach ($m in $DemoModules) {
        if (@($DemoTable | Where-Object { $_.Module -eq $m }).Count -eq 0) { Stop-Install 78 ('Unknown demo module ' + $m + '.') ('Known modules: ' + ((@($DemoTable | ForEach-Object { $_.Module })) -join ', ') + '.') }
      }
      $Target = 'canary'
    } elseif ($DemoModules.Count -gt 0 -or $DemoEnv.Count -gt 0) { Stop-Install 78 '-DemoModules and -DemoEnv go with -Demo.' 'Add -Demo, or leave them out.' }
    $me = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { Stop-Install 77 'Not running as Administrator.' 'Open PowerShell with "Run as administrator" and paste the command again.' }
    try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072 }
    catch { Warn 'TLS 1.2 is not available: install .NET Framework 4.5 or newer if the download fails.' }

    $v = [Environment]::OSVersion.Version
    if ($v.Major -lt 6 -or ($v.Major -eq 6 -and $v.Minor -lt 1)) { Stop-Install 78 ('Windows NT ' + $v + ' is too old.') 'Windows 7 / Server 2008 R2 or newer is needed.' }
    $os = 'windows'
    if ($v.Major -lt 10) { $os = 'windows7' }
    $arch = 'x86'
    if ($env:PROCESSOR_ARCHITECTURE -eq 'AMD64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'AMD64' -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { $arch = 'x86_64' }
    $os = $os + '-' + $arch

    $pf = $env:ProgramW6432
    if (-not $pf) { $pf = $env:ProgramFiles }
    $Exe = Join-Path $pf ('DevLibrary\' + $Product + '\' + $Product + '.exe')
    $Data = Join-Path $env:ProgramData $Product
    $InstallDir = Split-Path $Exe

    # The build to download: the target's, else -Channel's, else the channel this machine follows.
    $version = ''
    $lane = $Channel
    if ($Target -match '^\d') { $version = $Target; $lane = 'stable' }
    elseif ($Target -eq 'canary') { $lane = 'canary' }
    elseif ($Target -and $Target -ne 'auto') { $lane = 'stable' }
    elseif (-not $lane) { $lane = Get-InstalledChannel $Data }

    $info = Get-Info $lane $os $version
    $skew = (UnixNow) - [long]$info['server_time']
    if ([Math]::Abs($skew) -gt $MaxSkew) {
      Warn ('The clock is off by ' + $skew + ' s; trying to resync with w32tm.')
      & w32tm.exe /resync /force | Out-Null
      $info = Get-Info $lane $os $version
      $skew = (UnixNow) - [long]$info['server_time']
      if ([Math]::Abs($skew) -gt $MaxSkew) { Stop-Install 78 ('The clock is off by ' + $skew + ' s; the hub refuses agents more than 5 min off.') 'Set the date, time and time zone of this PC, then run again.' }
    }
    # The download lands in the install folder (Administrators only), not %TEMP%: an unsigned
    # program run from %TEMP% that installs a service is a dropper to behaviour engines (RT-0361).
    foreach ($d in @($InstallDir, $Data)) { if (-not (Test-Path $d)) { [void](New-Item -ItemType Directory -Force -Path $d) } }
    Set-AvTrust $InstallDir $Data
    Say ('Downloading ' + $Product + ' ' + $info['version'] + ' (' + $info['channel'] + ', ' + $os + ') from ' + $Hub)

    $tmp = Join-Path $InstallDir ($Product + '-setup-' + [Guid]::NewGuid().ToString('N').Substring(0, 8) + '.exe')
    try { (New-Client).DownloadFile($Hub + $info['download'], $tmp) }
    catch { $ex = Get-WebError $_; Stop-Install 69 ($Unreachable + ' (download: ' + $ex.Message + ')') 'Run again; if it repeats, allow the hub in the proxy or firewall.' }
    if (-not (Test-Path $tmp)) { Start-Sleep -Seconds 2 }
    if (-not (Test-Path $tmp)) { Show-AvBlock; Stop-Install 69 'The download was removed right after it was written (antivirus).' 'Restore it from the antivirus quarantine or allow the hub''s downloads, then run again.' }
    $size = (Get-Item $tmp).Length
    if ($size -ne [long]$info['size']) { Stop-Install 65 ('The download is ' + $size + ' bytes, expected ' + $info['size'] + '.') 'Run again; if it repeats, a proxy is changing downloads.' }
    $sha = Get-Sha256 $tmp
    if ($sha -ne $info['sha256'].ToLower()) { Stop-Install 65 ('SHA-256 mismatch: got ' + $sha + ', expected ' + $info['sha256'] + '.') 'Run again; if it repeats, a proxy is changing downloads.' }

    if (-not (Test-Modern $tmp)) {
      if ($version) { Stop-Install 78 ($Product + ' ' + $version + ' predates version pinning.') 'Install a channel instead: stable or canary.' }
      if ($DlState.Demo) { Stop-Install 78 ($Product + ' ' + $info['version'] + ' predates -Demo.') 'Publish a newer canary build, then run again.' }
      Say 'This build predates the new installer: the previous installer runs it.'
      Invoke-Legacy $lane
      return
    }

    # The release signature (Ed25519) over the manifest that names this SHA-256. An installed agent
    # that knows verify-self checks it (trust carried from the previous install); on a new PC the
    # downloaded binary checks itself against the release keys built into it.
    $manifest = Get-HubText ('/install/manifest/' + $Product + '/' + $info['channel'] + '/' + $info['version'])
    if ($manifest -notmatch '^\s*\{') { Stop-Install 69 ($Unreachable + ': the release manifest is not JSON') ('Open ' + $Hub + '/healthz in a browser on this PC, then run again.') }
    $mf = $tmp + '.manifest.json'
    [IO.File]::WriteAllText($mf, $manifest)
    $verifier = $tmp
    if (Test-Modern $Exe) { $verifier = $Exe }
    & $verifier verify-self --manifest $mf --file $tmp --target $os --product $Product
    if ($LASTEXITCODE -ne 0) { Stop-Install 65 'The download failed the release signature check; it was not run.' 'Run again; if it repeats, tell the hub admin (a proxy or an attacker may be changing downloads).' }

    # Hand-off: the binary does the machine setup. Secrets travel in env only, removed below.
    $hubs = $Hub
    foreach ($h in ('__HUBS__' -split ',')) { if ($h -and (($hubs -split ',') -notcontains $h)) { $hubs += ',' + $h } }
    $argv = @('install')
    if ($Target) { $argv += $Target }
    $argv += @('--hub', $hubs, '--data', $Data, '--link-timeout', [string]$LinkTimeout)
    if ($SqlName) { $argv += @('--sql-name', $SqlName) }
    if ($SqlAuth) { $argv += @('--sql-auth', $SqlAuth) }
    if ($SqlServer) { $argv += @('--sql-server', $SqlServer) }
    if ($SqlUser) { $argv += @('--sql-user', $SqlUser) }
    if ($SqlPrompt) { $argv += '--sql-prompt' }
    if ($Reenroll) { $argv += '--reenroll' }
    $argv += '--no-defender'
    if ($Setup) { $argv += @('--setup', $Setup) }
    foreach ($r in $Replace) { if ($r) { $argv += @('--replace', $r) } }
    if ($Code) { $env:DL_AGENT_ENROLL_CODE = $Code }
    if ($SqlConnection) { $env:DL_AGENT_SQL_CONNECTION = $SqlConnection }
    if (-not (Test-Path (Join-Path $Data 'sql.json'))) {
      $legacy = Get-LegacySqlConnection
      if ($legacy) { $env:DL_AGENT_SQL_LEGACY = $legacy; $env:DL_AGENT_SQL_LEGACY_FROM = $St.SqlFrom }
      $legacy = $null
    }
    & $tmp @argv
    $rc = $LASTEXITCODE
    if ($rc -ne 0) { Stop-Install $rc ('The install stopped (exit ' + $rc + ').') 'Fix what the agent named above, then run the same command again: it keeps this machine''s identity.' }
    $DlState.Done = $true
  } catch {
    $rc = 1
    $next = 'Run the same command again; it keeps this machine''s identity.'
    if ($_.Exception.Data.Contains('code')) { $rc = [int]$_.Exception.Data['code']; $next = [string]$_.Exception.Data['next'] }
    Write-Host ('[dl-agent] FAILED: ' + $_.Exception.Message) -ForegroundColor Red
    if ($next) { Write-Host ('[dl-agent] Next: ' + $next) -ForegroundColor Yellow }
    $DlState.Exit = $rc
  } finally {
    foreach ($f in @($tmp, $mf)) {
      if ($f -and (Test-Path $f)) {
        try { Remove-Item -Force $f } catch { Warn ('Could not remove the temporary file ' + $f + ': ' + $_.Exception.Message) }
      }
    }
    foreach ($n in ($HandoffSecrets + $SetupSecrets)) { Remove-Item ('Env:' + $n) -ErrorAction SilentlyContinue }
  }
}

__DEMO_BLOCK__
Install-Agent
if ($DlState.Demo) {
  try { if ($DlState.Done) { Invoke-Demo } }
  catch { Write-Host ('[dl-agent] Demo FAILED: ' + (Hide-Demo $_.Exception.Message)) -ForegroundColor Red; $DlState.Exit = 1 }
  finally { Clear-DemoEnv }
}
$global:LASTEXITCODE = $DlState.Exit
if ($DlState.Done) {
  $ok = '[dl-agent]'
  if ($PSVersionTable.PSVersion.Major -ge 5) { $ok = [string][char]0x2705 }
  Write-Host ''
  Write-Host ($ok + ' Installation complete') -ForegroundColor Green
}
# Unattended runs (RMM, SYSTEM, -NonInteractive, -SqlAuth) report failure through the exit
# code; an interactive console stays open.
$batch = (-not [Environment]::UserInteractive) -or [bool]$SqlAuth -or (@([Environment]::GetCommandLineArgs() | Where-Object { $_ -match '^-noni' }).Count -gt 0)
if ($DlState.Exit -ne 0 -and $batch) { exit $DlState.Exit }
`;

const ARRIVAL_START = '#<arrival-sql>\n';
const ARRIVAL_END = '#</arrival-sql>\n';

/**
 * The installer with this hub's origin as its default hub, `hubs` (HUB_URLS) as the enrolment
 * fallbacks and `arrivalSql` as the `Get-ArrivalLegacySql` body (by default the old Check-In
 * agent's connection reader from `./arrival`).
 */
export function installScript(origin: string, hubs: readonly string[] = [], arrivalSql: string = ARRIVAL_LEGACY_SQL_BODY): string {
  return SCRIPT.split('__DEMO_BLOCK__')
    .join(DEMO_BLOCK.split('__DEMO_MODULES__').join(demoModulesPs()))
    .split('__ARRIVAL_SQL__')
    .join(arrivalSql)
    .split('__HUBS__')
    .join(hubs.join(','))
    .split('__HUB__')
    .join(origin);
}

/** The `Get-ArrivalLegacySql` body `installScript` splices in (whatever its default is). */
export function arrivalSqlOf(script: string): string {
  const start = script.indexOf(ARRIVAL_START);
  const end = script.indexOf(ARRIVAL_END, start);
  if (start < 0 || end < 0) throw new Error('install script without the arrival block');
  const block = script.slice(start + ARRIVAL_START.length, end).split('\n');
  // The block is $St, Add-Secret, then the body.
  return block.slice(2).join('\n').replace(/\n$/, '');
}

/** The previous installer (`/install/legacy.ps1`) with the same arrival body as `/install.ps1`. */
export function legacyScript(origin: string, hubs: readonly string[] = []): string {
  return legacyInstallScript(origin, hubs, arrivalSqlOf(installScript(origin, hubs)));
}

/** The one-liner an admin pastes (PowerShell 2.0 safe; the code stays out of the URL). */
export function installCommand(origin: string, code: string): string {
  return (
    '[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072; ' +
    `& ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('${origin}/install.ps1'))) -Code '${code}'`
  );
}

/** The demo bench one-liner (`POST /admin/demo-bench`): canary, `-Demo`, the one-time code; module codes stay in env. */
export function demoCommand(origin: string, code: string, modules?: readonly string[]): string {
  const pick = modules && modules.length > 0 ? ` -DemoModules ${modules.map((m) => `'${m}'`).join(',')}` : '';
  return `${installCommand(origin, code)} -Demo${pick}`;
}
