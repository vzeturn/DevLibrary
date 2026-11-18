/**
 * The Windows installer served at `/install.ps1` (RT-0164 C8). It runs on PowerShell 2.0
 * (Windows 7 / Server 2008 R2): no JSON cmdlets, no Get-FileHash, no Invoke-RestMethod.
 * Kept free of backticks and dollar-brace so it can live in a raw template literal; the hub
 * origin replaces `__HUB__`.
 *
 * Steps: admin + TLS 1.2 → OS gate (NT < 10 → windows7 build, x86/x64) → clock check against
 * the hub → Defender exclusion → download + size + SHA-256 → stop and remove older copies
 * (the old exe uninstalls itself, other services are deleted) → optional SQL Server prompt,
 * tested before the code is spent → enroll unless already enrolled (re-run keeps the identity)
 * → optional product setup sub-command (`-Setup`) → remove the product's previous agent
 * (`-Replace`) only now, so a failure before this point leaves the old agent running → install
 * the service + watchdog → wait until the agent's heartbeat file shows a hub contact.
 */
const SCRIPT = String.raw`# DevLibrary Agent installer. Usage (PowerShell as Administrator):
#   [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072
#   & ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('__HUB__/install.ps1'))) -Code 'XXXX-XXXX'
# Options: -Channel canary, -SqlPrompt (SQL Server on another machine), -SqlName/-SqlConnection,
#          -Reenroll (replace the identity), -NoDefender, -LinkTimeout <seconds>,
#          -Setup <sub-command> (run after enroll; secrets reach it through env DL_AGENT_SETUP_ARGS,
#          removed afterwards), -Replace <service name pattern> (an older per-product agent: its
#          services are stopped and deleted and its processes ended; its files and config stay).
param(
  [string]$Code = $env:DL_AGENT_ENROLL_CODE,
  [string]$Hub = '__HUB__',
  [ValidateSet('stable', 'canary')][string]$Channel = 'stable',
  [string]$Product = 'dl-agent',
  [string]$SqlName = '',
  [string]$SqlConnection = '',
  [switch]$SqlPrompt,
  [switch]$Reenroll,
  [switch]$NoDefender,
  [int]$LinkTimeout = 120,
  [string]$Setup = '',
  [string[]]$Replace = @()
)

$ErrorActionPreference = 'Stop'
$MaxSkew = 240

function Say([string]$m) { Write-Host ('[dl-agent] ' + $m) }
function Warn([string]$m) { Write-Host ('[dl-agent] ' + $m) -ForegroundColor Yellow }
function UnixNow { [long]([DateTime]::UtcNow - (New-Object DateTime 1970, 1, 1, 0, 0, 0, ([DateTimeKind]::Utc))).TotalSeconds }

function New-Client {
  $wc = New-Object Net.WebClient
  $wc.Headers.Add('Cache-Control', 'no-cache')
  $wc.Encoding = [Text.Encoding]::UTF8
  return $wc
}

function Get-HubText([string]$path) {
  try { return (New-Client).DownloadString($Hub + $path) }
  catch [Net.WebException] {
    $body = ''
    if ($_.Exception.Response) {
      $r = New-Object IO.StreamReader($_.Exception.Response.GetResponseStream())
      $body = $r.ReadToEnd()
      $r.Close()
    }
    throw ('hub ' + $path + ' failed: ' + $_.Exception.Message + ' ' + $body)
  }
}

function Get-Info([string]$target) {
  $info = @{}
  foreach ($line in ((Get-HubText ('/install/info/' + $Product + '/' + $Channel + '/' + $target)) -split '\r?\n')) {
    $i = $line.IndexOf('=')
    if ($i -gt 0) { $info[$line.Substring(0, $i)] = $line.Substring($i + 1).Trim() }
  }
  if (-not $info['sha256']) { throw ('hub answered without a build for ' + $target) }
  return $info
}

function Get-Sha256([string]$path) {
  $s = [IO.File]::OpenRead($path)
  try { $h = [Security.Cryptography.SHA256]::Create().ComputeHash($s) } finally { $s.Close() }
  return ([BitConverter]::ToString($h) -replace '-', '').ToLower()
}

function Invoke-Agent([string[]]$argv) {
  & $Exe @argv | Out-Host
  return ($LASTEXITCODE -eq 0)
}

function Add-Sql([string]$name, [string]$conn) {
  $env:DL_AGENT_SQL_CONNECTION = $conn
  try { $ok = Invoke-Agent @('sql-add', $name, '--data', $Data) } finally { Remove-Item Env:DL_AGENT_SQL_CONNECTION -ErrorAction SilentlyContinue }
  if (-not $ok) { return $false }
  return (Invoke-Agent @('sql-test', $name, '--data', $Data))
}

function Ask([string]$label, [string]$default) {
  $a = Read-Host ($label + ' [' + $default + ']')
  if ($a) { return $a } else { return $default }
}

function Quote([string]$v) { return '"' + ($v -replace '"', '""') + '"' }

function Read-SqlDatabases {
  $d = @{ name = 'SMILE_FO'; server = ''; database = ''; user = 'sa' }
  while ($true) {
    $d.name = Ask 'Database key (SMILE_FO, SMILE_POS, PDA...)' $d.name
    $d.server = Ask 'SQL Server (host\instance or host,port)' $d.server
    $d.database = Ask 'Database name' $d.database
    $d.user = Ask 'SQL login' $d.user
    $sec = Read-Host 'Password' -AsSecureString
    $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($sec)
    try { $pw = [Runtime.InteropServices.Marshal]::PtrToStringAuto($bstr) } finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
    $conn = 'Server=' + (Quote $d.server) + ';Database=' + (Quote $d.database) + ';User Id=' + (Quote $d.user) + ';Password=' + (Quote $pw) + ';'
    if (Add-Sql $d.name $conn) {
      Say ('SQL ' + $d.name + ' connected.')
      if ((Read-Host 'Add another database? (y/N)') -ne 'y') { return }
      $d.name = ''
    } else {
      Warn 'Connection failed (see above). Enter the values again; Enter keeps the previous answer.'
    }
  }
}

function Remove-OldCopies {
  $svc = $null
  foreach ($s in @(Get-WmiObject Win32_Service | Where-Object { $_.PathName -like ('*\' + $Product + '.exe*') })) {
    if ($s.Name -eq $Product) { $svc = $s; continue }
    Warn ('Removing older copy: service ' + $s.Name + ' (' + $s.PathName + ')')
    if ($s.State -ne 'Stopped') { $s.StopService() | Out-Null; Start-Sleep -Seconds 3 }
    & sc.exe delete $s.Name | Out-Null
  }
  if ($svc) {
    Say ('Stopping service ' + $Product)
    if ($svc.PathName -notlike ('*' + $Exe + '*')) { Warn ('Service pointed at ' + $svc.PathName + '; repairing the path.') }
    $removed = $false
    if (Test-Path $Exe) { $removed = Invoke-Agent @('uninstall', '--data', $Data) }
    if (-not $removed) {
      if ($svc.State -ne 'Stopped') { $svc.StopService() | Out-Null; Start-Sleep -Seconds 3 }
      & sc.exe delete $Product | Out-Null
    }
  }
  foreach ($p in @(Get-Process -Name $Product -ErrorAction SilentlyContinue)) {
    Warn ('Stopping leftover process ' + $p.Id)
    Stop-Process -Id $p.Id -Force
  }
  foreach ($f in @(($Exe + '.new'), ($Exe + '.download'))) { if (Test-Path $f) { Remove-Item $f -Force } }
}

function Remove-Replaced {
  foreach ($pattern in $Replace) {
    if (-not $pattern) { continue }
    foreach ($s in @(Get-WmiObject Win32_Service | Where-Object { ($_.Name -like $pattern -or $_.DisplayName -like $pattern) -and $_.PathName -notlike ('*' + $Exe + '*') })) {
      Warn ('Replacing ' + $s.Name + ' (' + $s.PathName + '); its files are kept.')
      if ($s.State -ne 'Stopped') { $s.StopService() | Out-Null; Start-Sleep -Seconds 3 }
      & sc.exe delete $s.Name | Out-Null
      if ($LASTEXITCODE -ne 0) { throw ('Could not delete service ' + $s.Name + ' (sc.exe exit ' + $LASTEXITCODE + ').') }
      if ($s.PathName -match '^\s*"([^"]+)"') { $path = $matches[1] } else { $path = ($s.PathName -split ' ')[0] }
      foreach ($p in @(Get-WmiObject Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath -eq $path })) {
        Warn ('Ending process ' + $p.ProcessId + ' (' + $p.ExecutablePath + ')')
        $p.Terminate() | Out-Null
      }
    }
  }
}

function Wait-Link([long]$since) {
  $beat = Join-Path $Data 'heartbeat.json'
  $deadline = (UnixNow) + $LinkTimeout
  while ((UnixNow) -lt $deadline) {
    if (Test-Path $beat) {
      $m = [regex]::Match([IO.File]::ReadAllText($beat), '"hub_at"\s*:\s*(\d+)')
      if ($m.Success -and ([long]$m.Groups[1].Value) -ge $since) { return $true }
    }
    Start-Sleep -Seconds 3
  }
  return $false
}

try {
  $me = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
  if (-not $me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run PowerShell as Administrator.' }
  try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072 }
  catch { Warn 'TLS 1.2 is not available: install .NET Framework 4.5 or newer if the download fails.' }

  $v = [Environment]::OSVersion.Version
  if ($v.Major -lt 6 -or ($v.Major -eq 6 -and $v.Minor -lt 1)) { throw ('Windows NT ' + $v + ' is too old: Windows 7 / Server 2008 R2 or newer is needed.') }
  $os = 'windows'
  if ($v.Major -lt 10) { $os = 'windows7' }
  $arch = 'x86'
  if ($env:PROCESSOR_ARCHITECTURE -eq 'AMD64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'AMD64' -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { $arch = 'x86_64' }
  $target = $os + '-' + $arch

  $pf = $env:ProgramW6432
  if (-not $pf) { $pf = $env:ProgramFiles }
  $InstallDir = Join-Path $pf ('DevLibrary\' + $Product)
  $Exe = Join-Path $InstallDir ($Product + '.exe')
  $Data = Join-Path $env:ProgramData $Product
  Say ('Windows NT ' + $v + ', build ' + $target + ', channel ' + $Channel + ', hub ' + $Hub)

  $info = Get-Info $target
  $skew = (UnixNow) - [long]$info['server_time']
  if ([Math]::Abs($skew) -gt $MaxSkew) {
    Warn ('The clock is off by ' + $skew + ' s; trying to resync with w32tm.')
    & w32tm.exe /resync /force | Out-Null
    $info = Get-Info $target
    $skew = (UnixNow) - [long]$info['server_time']
    if ([Math]::Abs($skew) -gt $MaxSkew) { throw ('The clock is off by ' + $skew + ' s and the hub refuses agents more than 5 min off. Set the date, time and time zone, then run again.') }
  }

  New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
  New-Item -ItemType Directory -Force -Path $Data | Out-Null
  if (-not $NoDefender -and (Get-Command Add-MpPreference -ErrorAction SilentlyContinue)) {
    try { Add-MpPreference -ExclusionPath @($InstallDir, $Data); Say 'Windows Defender: agent folders excluded.' }
    catch { Warn ('Windows Defender exclusion not added: ' + $_.Exception.Message) }
  }

  Say ('Downloading ' + $Product + ' ' + $info['version'] + ' (' + $info['size'] + ' bytes)')
  $tmp = $Exe + '.download'
  (New-Client).DownloadFile($Hub + $info['download'], $tmp)
  $size = (Get-Item $tmp).Length
  if ($size -ne [long]$info['size']) { Remove-Item $tmp -Force; throw ('Download is ' + $size + ' bytes, expected ' + $info['size'] + '. Run again.') }
  $sha = Get-Sha256 $tmp
  if ($sha -ne $info['sha256'].ToLower()) { Remove-Item $tmp -Force; throw ('SHA-256 mismatch: got ' + $sha + ', expected ' + $info['sha256'] + '. Run again.') }

  Remove-OldCopies
  Move-Item -Force $tmp $Exe
  Say ('Installed ' + $Exe + ' (SHA-256 ' + $sha + ')')

  if ($SqlName -and $SqlConnection) {
    if (-not (Add-Sql $SqlName $SqlConnection)) { throw ('SQL ' + $SqlName + ' did not connect; nothing was enrolled.') }
  } elseif ($SqlPrompt) {
    Read-SqlDatabases
  }

  $enrolled = Test-Path (Join-Path $Data 'identity.json')
  if ($enrolled -and -not $Reenroll) {
    Say 'Already enrolled: keeping this machine''s identity.'
  } else {
    if (-not $Code) { $Code = Read-Host 'Enrolment code' }
    if (-not $Code) { throw 'An enrolment code is needed (ask the hub admin).' }
    $argv = @('enroll', '--hub', $Hub, '--data', $Data)
    if ($enrolled) { $argv += '--force' }
    $env:DL_AGENT_ENROLL_CODE = $Code
    try { $ok = Invoke-Agent $argv } finally { Remove-Item Env:DL_AGENT_ENROLL_CODE -ErrorAction SilentlyContinue }
    if (-not $ok) { throw 'Enrolment failed (see above). The code may be used, expired or revoked.' }
  }

  if ($Setup) {
    Say ('Running setup: ' + $Setup)
    try { $ok = Invoke-Agent @($Setup, '--data', $Data) } finally { Remove-Item Env:DL_AGENT_SETUP_ARGS -ErrorAction SilentlyContinue }
    if (-not $ok) { throw ('Setup ' + $Setup + ' failed (see above). The previous agent, if any, is still running.') }
  }
  Remove-Replaced

  $since = (UnixNow) - 5
  if (-not (Invoke-Agent @('install', '--data', $Data))) { throw 'Service install failed (see above).' }

  $kav = @(Get-Service -ErrorAction SilentlyContinue | Where-Object { $_.Name -like 'AVP*' -or $_.Name -eq 'klnagent' -or $_.DisplayName -like '*Kaspersky*' })
  if ($kav.Count -gt 0) {
    Warn ('Kaspersky is installed. In Kaspersky (or Security Center policy) add ' + $Exe + ' as a Trusted application (SHA-256 ' + $sha + ') and exclude ' + $Data + ' from scanning.')
  }

  Say ('Waiting up to ' + $LinkTimeout + ' s for the agent to reach the hub...')
  if (Wait-Link $since) {
    Write-Host '[dl-agent] Connected to the hub.' -ForegroundColor Green
  } else {
    Write-Host ('[dl-agent] NOT connected after ' + $LinkTimeout + ' s. Last log lines:') -ForegroundColor Red
    $log = Join-Path $Data 'logs\agent.log'
    if (Test-Path $log) { Get-Content $log | Select-Object -Last 15 | ForEach-Object { Write-Host $_ } }
    & $Exe status --data $Data | Out-Host
  }
} catch {
  Write-Host ('[dl-agent] FAILED: ' + $_.Exception.Message) -ForegroundColor Red
}
`;

/** The installer with this hub's origin as its default hub. */
export function installScript(origin: string): string {
  return SCRIPT.split('__HUB__').join(origin);
}

/** The one-liner an admin pastes (PowerShell 2.0 safe; the code stays out of the URL). */
export function installCommand(origin: string, code: string): string {
  return (
    '[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072; ' +
    `& ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('${origin}/install.ps1'))) -Code '${code}'`
  );
}
