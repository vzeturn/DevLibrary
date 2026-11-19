/**
 * The previous Windows installer (RT-0164 C8), served at `/install/legacy.ps1`. `/install.ps1`
 * hands off to it when the build it downloaded predates `dl-agent install` / `verify-self`, so
 * machines keep installing until a release with the new verbs ships. It runs on PowerShell 2.0
 * (Windows 7 / Server 2008 R2): no JSON cmdlets, no Get-FileHash, no Invoke-RestMethod.
 * Kept free of backticks and dollar-brace so it can live in a raw template literal; the hub
 * origin replaces `__HUB__`.
 *
 * Steps: admin + TLS 1.2 → OS gate (NT < 10 → windows7 build, x86/x64) → clock check against
 * the hub → Defender exclusion → download + size + SHA-256 → stop and remove older copies
 * (the old exe uninstalls itself, other services are deleted) → SQL login (PROTOCOL.md §9.8:
 * Windows or SQL login, defaulting to what this machine saved, tested on every SMILE database as
 * the service account before anything is saved, and before the code is spent; with nothing saved, the
 * old Check-In agent's connection from `Get-ArrivalLegacySql`) → enroll unless already enrolled (re-run
 * keeps the identity) → optional product setup sub-command (`-Setup`; `arrival-enroll` also gets the SQL
 * login through env) → remove the product's previous agent
 * (`-Replace`) only now, so a failure before this point leaves the old agent running → install
 * the service + watchdog → wait until the agent's heartbeat file shows a hub contact.
 */
const SCRIPT = String.raw`# DevLibrary Agent installer. Usage (PowerShell as Administrator):
#   [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072
#   & ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('__HUB__/install.ps1'))) -Code 'XXXX-XXXX'
# SQL login: asked when run interactively on a machine with SQL Server (or with -SqlPrompt):
#   "SQL login: [1] Windows  [2] SQL login"; Enter keeps what this machine saved.
#   Unattended: -SqlAuth windows|sql [-SqlServer .\SQLEXPRESS] [-SqlUser smile]; the password only
#   from env DL_AGENT_SQL_PASSWORD or, run interactively, a hidden prompt (never a parameter,
#   echoed or logged; sealed with DPAPI).
#   A failed test prints the exact SQL error and exits 1; nothing is saved. With nothing saved
#   the old Check-In agent's connection (Get-ArrivalLegacySql) is the default, its password included.
#   With -Setup arrival-enroll the same -SqlAuth/-SqlUser and password reach it through env.
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
  [ValidateSet('', 'windows', 'sql')][string]$SqlAuth = '',
  [string]$SqlServer = '',
  [string]$SqlUser = '',
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

# Called when a just-written file vanished: names the active antivirus and Defender's latest detection.
function Show-AvBlock([string]$path) {
  try {
    foreach ($a in @(Get-WmiObject -Namespace root\SecurityCenter2 -Class AntivirusProduct -ErrorAction Stop)) { Warn ('Antivirus on this PC: ' + $a.displayName) }
  } catch { Warn 'Antivirus list not available (Windows Server has no Security Center).' }
  if (Get-Command Get-MpThreatDetection -ErrorAction SilentlyContinue) {
    try {
      $d = @(Get-MpThreatDetection -ErrorAction Stop | Sort-Object InitialDetectionTime -Descending | Select-Object -First 1)
      if ($d.Count -gt 0) {
        $t = @(Get-MpThreat -ThreatID $d[0].ThreatID -ErrorAction SilentlyContinue)
        $name = 'id ' + $d[0].ThreatID
        if ($t.Count -gt 0) { $name = $t[0].ThreatName }
        Warn ('Windows Defender latest detection: ' + $name + ' at ' + $d[0].InitialDetectionTime + ' on ' + ($d[0].Resources -join ', '))
      }
    } catch { Warn ('Windows Defender detections not readable: ' + $_.Exception.Message) }
  }
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

function Read-Secret([string]$label) {
  $sec = Read-Host $label -AsSecureString
  $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($sec)
  try { return [Runtime.InteropServices.Marshal]::PtrToStringAuto($bstr) } finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
}

# -- arrival legacy SQL (RT-0164): the old Check-In agent's SQL Server connection is the saved
#    answer when this machine saved none. Get-ArrivalLegacySql (the arrival slice's body from
#    src/install/arrival.ts by default) is spliced below; it needs $St,
#    Add-Secret and Warn. This script prints no transcript, so Add-Secret only keeps the list.
$St = @{ SqlFrom = ''; Secrets = @() }
function Add-Secret([string]$s) { if ($s) { $St.Secrets += $s } }
__ARRIVAL_SQL__
# Fills $s from the old agent's connection: mode, server, login and, in memory only, its password.
function Add-LegacySql($s) {
  $conn = [string](Get-ArrivalLegacySql)
  if (-not $conn) { return }
  try { $b = New-Object Data.SqlClient.SqlConnectionStringBuilder $conn } catch { Warn ('The old Check-In agent''s SQL connection was not understood: ' + $_.Exception.Message); return }
  $s.server = $b.DataSource
  $s.from = $St.SqlFrom
  if ($b.IntegratedSecurity) { $s.auth = 'windows' } else { $s.auth = 'sql'; $s.user = $b.UserID; $s.password = $b.Password }
}
# -- end arrival legacy SQL

# What this machine saved (sql-setup --show: auth, server, user, databases; never the password),
# else what the old Check-In agent used (from and password set only then).
function Get-SavedSql {
  $s = @{ auth = ''; server = ''; user = ''; databases = ''; from = ''; password = '' }
  foreach ($l in @(& $Exe sql-setup --show --data $Data 2>$null)) {
    $i = ([string]$l).IndexOf('=')
    if ($i -gt 0) { $s[([string]$l).Substring(0, $i)] = ([string]$l).Substring($i + 1) }
  }
  if (-not $s.auth) { Add-LegacySql $s }
  return $s
}

function Get-LocalSqlServer {
  $svc = @(Get-Service -ErrorAction SilentlyContinue | Where-Object { $_.Name -eq 'MSSQLSERVER' -or $_.Name -like 'MSSQL$*' })
  if ($svc.Count -eq 0) { return '' }
  if (@($svc | Where-Object { $_.Name -eq 'MSSQLSERVER' }).Count -gt 0) { return '.' }
  return ('.\' + $svc[0].Name.Substring(6))
}

# Tests the login on every SMILE database (Windows: as LocalSystem, the service account) and
# saves it only when all connect. The password travels in env DL_AGENT_SQL_PASSWORD, removed after.
function Invoke-SqlSetup([string]$auth, [string]$server, [string]$user, [string]$pw) {
  $argv = @('sql-setup', '--auth', $auth, '--server', $server, '--data', $Data, '--result', $SqlResult)
  if ($auth -eq 'sql') { $argv += @('--user', $user) } else { $argv += '--as-service' }
  if ($pw) { $env:DL_AGENT_SQL_PASSWORD = $pw }
  try { & $Exe @argv | Out-Host; $code = $LASTEXITCODE } finally { Remove-Item Env:DL_AGENT_SQL_PASSWORD -ErrorAction SilentlyContinue }
  return $code
}

function Read-SqlLogin {
  $saved = Get-SavedSql
  $mode = '1'
  if ($saved.auth -eq 'sql') { $mode = '2' }
  $server = $saved.server
  if (-not $server) { $server = Get-LocalSqlServer }
  if (-not $server) { $server = '.' }
  $user = $saved.user
  if (-not $user) { $user = 'sa' }
  if ($saved.from) {
    Say ($saved.from + ': ' + $saved.auth + ' login on ' + $saved.server + '. Enter keeps it.')
  } elseif ($saved.auth) {
    Say ('Saved SQL login: ' + $saved.auth + ' on ' + $saved.server + ' (' + $saved.databases + '). Enter keeps it.')
  }
  while ($true) {
    $mode = Ask 'SQL login: [1] Windows  [2] SQL login  [0] skip' $mode
    if ($mode -eq '0') { Warn 'SQL login not changed.'; return }
    if ($mode -ne '1' -and $mode -ne '2') { Warn 'Type 1, 2 or 0.'; $mode = '1'; continue }
    $server = Ask 'SQL Server (., .\SQLEXPRESS, host\instance or host,port)' $server
    $pw = ''
    $auth = 'windows'
    if ($mode -eq '2') {
      $auth = 'sql'
      $user = Ask 'SQL login name' $user
      if ($saved.auth -eq 'sql' -and $user -eq $saved.user) { $pw = Read-Secret 'Password (Enter keeps the saved one)' } else { $pw = Read-Secret 'Password' }
      if (-not $pw -and $saved.password -and $user -eq $saved.user) { $pw = $saved.password }
    }
    $code = Invoke-SqlSetup $auth $server $user $pw
    $pw = $null
    if ($code -eq 0) { Say 'SQL login tested and saved.'; return }
    Warn 'The SQL login test failed (exact error above). Choose again; Enter keeps the previous answer.'
  }
}

# Unattended (-SqlAuth): unset values fall back to what this machine saved. Returns $false on a
# failed test after printing the structured result (sql-setup.last.json). $Password holds the
# password from env DL_AGENT_SQL_PASSWORD; a password typed at the prompt is handed back in it.
function Set-SqlLoginUnattended([ref]$Password) {
  $saved = Get-SavedSql
  $server = $SqlServer
  if (-not $server) { $server = $saved.server }
  if (-not $server) { $server = Get-LocalSqlServer }
  if (-not $server) { $server = '.' }
  $user = $SqlUser
  if (-not $user) { $user = $saved.user }
  $pw = $Password.Value
  if (-not $pw -and $SqlAuth -eq 'sql' -and $saved.password -and $user -eq $saved.user) {
    Say 'No DL_AGENT_SQL_PASSWORD: using the old Check-In agent''s password for this login.'
    $pw = $saved.password
  } elseif (-not $pw -and $SqlAuth -eq 'sql' -and [Environment]::UserInteractive) {
    $pw = Read-Secret ('Password of SQL login ' + $user)
    $Password.Value = $pw
  }
  $code = Invoke-SqlSetup $SqlAuth $server $user $pw
  $pw = $null
  if ($code -eq 0) { Say ('SQL login ' + $SqlAuth + ' tested and saved.'); return $true }
  if (Test-Path $SqlResult) { Write-Host ('[dl-agent] SQL_SETUP_RESULT ' + [IO.File]::ReadAllText($SqlResult).Trim()) }
  return $false
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
    # A newer agent's uninstall also deletes its files: --keep-files keeps the folder this
    # script downloads into (older agents do not know the option and keep them anyway).
    if (Test-Path $Exe) {
      $argv = @('uninstall', '--data', $Data)
      if ((& $Exe uninstall --help | Out-String) -match 'keep-files') { $argv += '--keep-files' }
      $removed = Invoke-Agent $argv
    }
    if (-not $removed) {
      if ($svc.State -ne 'Stopped') { $svc.StopService() | Out-Null; Start-Sleep -Seconds 3 }
      & sc.exe delete $Product | Out-Null
    }
  }
  foreach ($p in @(Get-Process -Name $Product -ErrorAction SilentlyContinue)) {
    Warn ('Stopping leftover process ' + $p.Id)
    Stop-Process -Id $p.Id -Force
  }
  # Not $Exe.download: it is the verified download that Move-Item installs next.
  if (Test-Path ($Exe + '.new')) { Remove-Item ($Exe + '.new') -Force }
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
  $SqlResult = Join-Path $Data 'sql-setup.last.json'
  $SqlFailed = $false
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
  if (-not (Test-Path $tmp)) { Start-Sleep -Seconds 2 }
  if (-not (Test-Path $tmp)) { Show-AvBlock $tmp; throw ('The downloaded file was removed right after it was written (antivirus). Allow ' + $InstallDir + ' in the antivirus shown above, then run again.') }
  $size = (Get-Item $tmp).Length
  if ($size -ne [long]$info['size']) { Remove-Item $tmp -Force; throw ('Download is ' + $size + ' bytes, expected ' + $info['size'] + '. Run again.') }
  $sha = Get-Sha256 $tmp
  if ($sha -ne $info['sha256'].ToLower()) { Remove-Item $tmp -Force; throw ('SHA-256 mismatch: got ' + $sha + ', expected ' + $info['sha256'] + '. Run again.') }

  Remove-OldCopies
  Move-Item -Force $tmp $Exe
  Say ('Installed ' + $Exe + ' (SHA-256 ' + $sha + ')')

  $enrolled = Test-Path (Join-Path $Data 'identity.json')
  if ($SqlName -and $SqlConnection) {
    if (-not (Add-Sql $SqlName $SqlConnection)) { throw ('SQL ' + $SqlName + ' did not connect; nothing was enrolled.') }
  } elseif ($SqlAuth) {
    # -- SQL password: env DL_AGENT_SQL_PASSWORD or the hidden prompt, never a parameter
    $SqlPassword = $env:DL_AGENT_SQL_PASSWORD
    # -- end SQL password
    $SqlFailed = -not (Set-SqlLoginUnattended ([ref]$SqlPassword))
    # arrival-enroll takes the same sign-in (below); otherwise the password is dropped now.
    if ($Setup -ne 'arrival-enroll') { $SqlPassword = $null }
    # Nothing was saved: a new machine stops before the code is spent; a reinstall comes back
    # with its previous SQL settings and reports sql_setup_failed to the hub.
    if ($SqlFailed -and -not $enrolled) { throw 'The SQL login test failed (see above); nothing was saved or enrolled.' }
    if ($SqlFailed) { Warn 'The SQL login test failed (see above): the previous SQL settings are kept.' }
  } elseif ($SqlPrompt -or ([Environment]::UserInteractive -and ((Get-LocalSqlServer) -or (Test-Path (Join-Path $Data 'sql.json'))))) {
    Read-SqlLogin
  }

  if ($enrolled -and -not $Reenroll) {
    Say 'Already enrolled: keeping this machine''s identity.'
  } else {
    if (-not $Code) { $Code = Read-Host 'Enrolment code' }
    if (-not $Code) { throw 'An enrolment code is needed (ask the hub admin).' }
    # -- hub fallback list (PROTOCOL.md §2.1): the hub's other hosts follow -Hub, so enrolment
    #    still works while the primary host is down; the answer's hub_urls is what the agent keeps.
    $hubArg = $Hub
    foreach ($h in ('__HUBS__' -split ',')) { if ($h -and (($hubArg -split ',') -notcontains $h)) { $hubArg += ',' + $h } }
    # -- end hub fallback list
    $argv = @('enroll', '--hub', $hubArg, '--data', $Data)
    if ($enrolled) { $argv += '--force' }
    $env:DL_AGENT_ENROLL_CODE = $Code
    try { $ok = Invoke-Agent $argv } finally { Remove-Item Env:DL_AGENT_ENROLL_CODE -ErrorAction SilentlyContinue }
    if (-not $ok) { throw 'Enrolment failed (see above). The code may be used, expired or revoked.' }
  }

  if ($Setup) {
    Say ('Running setup: ' + $Setup)
    # -- arrival sign-in (RT-0164): -SqlAuth/-SqlUser and the password reach arrival-enroll through env
    #    only, never argv; the setup's secret env (token, PMS connection, tenant key, password) is
    #    removed right after it ran.
    $SetupSecrets = @('DL_AGENT_ARRIVAL_TOKEN', 'DL_AGENT_ARRIVAL_PMS', 'DL_AGENT_ARRIVAL_TENANT_KEY', 'DL_AGENT_ARRIVAL_SQL_AUTH', 'DL_AGENT_ARRIVAL_SQL_USER', 'DL_AGENT_SQL_PASSWORD', 'DL_AGENT_EINVOICE_TOKEN', 'DL_AGENT_PMS_CODE')
    if ($Setup -eq 'arrival-enroll') {
      if ($SqlAuth) { $env:DL_AGENT_ARRIVAL_SQL_AUTH = $SqlAuth }
      if ($SqlUser) { $env:DL_AGENT_ARRIVAL_SQL_USER = $SqlUser }
      if ($SqlPassword) { $env:DL_AGENT_SQL_PASSWORD = $SqlPassword }
    }
    $SqlPassword = $null
    # -- end arrival sign-in
    try { $ok = Invoke-Agent @($Setup, '--data', $Data) } finally {
      Remove-Item Env:DL_AGENT_SETUP_ARGS -ErrorAction SilentlyContinue
      foreach ($n in $SetupSecrets) { Remove-Item ('Env:' + $n) -ErrorAction SilentlyContinue }
    }
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
  if ($SqlFailed) { Write-Host '[dl-agent] FAILED: SQL login test (see above).' -ForegroundColor Red; exit 1 }
} catch {
  Write-Host ('[dl-agent] FAILED: ' + $_.Exception.Message) -ForegroundColor Red
  # Unattended runs report failure through the exit code.
  if ($SqlAuth) { exit 1 }
}
`;

/**
 * The previous installer with this hub's origin as its default hub, `hubs` (HUB_URLS) as the
 * enrolment fallbacks and `arrivalSql` as the `Get-ArrivalLegacySql` body (the same body as
 * `/install.ps1`, so the arrival slice's import reaches both).
 */
export function legacyInstallScript(origin: string, hubs: readonly string[], arrivalSql: string): string {
  return SCRIPT.split('__ARRIVAL_SQL__').join(arrivalSql).split('__HUBS__').join(hubs.join(',')).split('__HUB__').join(origin);
}
