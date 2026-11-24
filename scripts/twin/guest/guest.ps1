# Runs ON the twin instance (pushed by twin.sh, started through SSM). One phase per call; the
# only thing written to stdout is one JSON object, so the harness can store it as it is.
# No SQL login is created or changed here (smile/smilepms keep whatever the fixture gives them:
# nothing): the agent talks to the local instance with Windows authentication.
param(
  [Parameter(Mandatory)][ValidateSet('sql', 'install', 'upgrade', 'selftest', 'uninstall')][string]$Phase,
  [string]$SqlEdition = '2014',
  [string]$InstallerUrl = '',
  [string]$BaseInstallerUrl = '',
  [string]$Hub = '',
  [string]$CodeParam = ''
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$work = 'C:\twin'
$agent = Join-Path $work 'dl-agent.exe'
$log = New-Object System.Collections.Generic.List[string]
$ok = $true

function Note([string]$m) { $log.Add($m) | Out-Null }

# Runs a native command, keeps the last lines of its output, returns the exit code.
function Run([string]$exe, [string[]]$argv) {
  $out = & $exe @argv 2>&1 | Out-String
  Note ("{0} {1} -> exit {2}" -f (Split-Path $exe -Leaf), ($argv -join ' '), $LASTEXITCODE)
  $tail = ($out -split "`r?`n" | Select-Object -Last 15) -join "`n"
  if ($tail) { Note $tail }
  return $LASTEXITCODE
}

function Fetch([string]$url, [string]$dest) {
  [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
  Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $dest
}

function InstallAgent([string]$url) {
  Fetch $url $agent
  $a = @('install', '--sql-auth', 'windows', '--sql-server', '.\TWIN', '--link-timeout', '60')
  if ($Hub -and $CodeParam) {
    $code = (Get-SSMParameter -Name $CodeParam -WithDecryption $true).Value
    $env:DL_AGENT_ENROLL_CODE = $code
    $a += @('--hub', $Hub)
  } else {
    $a += '--skip-sql'
  }
  $rc = Run $agent $a
  Remove-Item Env:\DL_AGENT_ENROLL_CODE -ErrorAction SilentlyContinue
  return $rc
}

try {
  switch ($Phase) {
    'sql' {
      # Old SQL Server Express (instance TWIN) with Windows authentication only, then the
      # synthetic sample DB. The installer URL is the vendor's; adjust per edition if it moves.
      $urls = @{
        '2012' = 'https://download.microsoft.com/download/8/D/D/8DD7BDBA-CEF7-4D8E-8C16-D9F69527F909/ENU/x64/SQLEXPR_x64_ENU.exe'
        '2014' = 'https://download.microsoft.com/download/E/A/E/EAE6F7FC-767A-4038-A954-49B8B05D04EB/Express%2064BIT/SQLEXPR_x64_ENU.exe'
      }
      if (-not $urls.ContainsKey($SqlEdition)) { throw "unsupported SQL edition $SqlEdition" }
      Fetch $urls[$SqlEdition] "$work\sqlexpr.exe"
      $sysadmin = [Security.Principal.WindowsIdentity]::GetCurrent().Name
      $rc = Run "$work\sqlexpr.exe" @('/Q', '/ACTION=Install', '/FEATURES=SQLEngine', '/INSTANCENAME=TWIN',
        "/SQLSYSADMINACCOUNTS=$sysadmin", '/IACCEPTSQLSERVERLICENSETERMS', '/SECURITYMODE=Windows')
      if ($rc -ne 0 -and $rc -ne 3010) { throw "SQL Server setup failed ($rc)" }
      $sqlcmd = Get-ChildItem 'C:\Program Files\Microsoft SQL Server\*\Tools\Binn\sqlcmd.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
      if (-not $sqlcmd) { throw 'sqlcmd.exe not found after setup' }
      $rc = Run $sqlcmd.FullName @('-S', '.\TWIN', '-E', '-b', '-i', "$work\sample_smile.sql")
      if ($rc -ne 0) { throw "sample DB load failed ($rc)" }
      $v = & $sqlcmd.FullName -S '.\TWIN' -E -h -1 -Q 'SET NOCOUNT ON; SELECT CAST(SERVERPROPERTY(''ProductVersion'') AS varchar(32))' | Out-String
      Note ("sql version " + $v.Trim())
    }
    'install' {
      $first = if ($BaseInstallerUrl) { $BaseInstallerUrl } else { $InstallerUrl }
      if ((InstallAgent $first) -ne 0) { throw 'install failed' }
    }
    'upgrade' {
      if ((InstallAgent $InstallerUrl) -ne 0) { throw 'upgrade failed' }
      Note ("version after upgrade: " + ((& $agent status 2>&1 | Out-String).Trim()))
    }
    'selftest' {
      if ((Run $agent @('selftest', '--json', '--no-upload')) -ne 0) { throw 'selftest reported failures' }
    }
    'uninstall' {
      if (-not (Test-Path $agent)) { throw 'no agent to uninstall' }
      if ((Run $agent @('uninstall', '--purge')) -ne 0) { throw 'uninstall failed' }
      if (Get-Service -Name 'dl-agent*' -ErrorAction SilentlyContinue) { throw 'service still registered after uninstall' }
    }
  }
} catch {
  $ok = $false
  Note ("error: " + $_.Exception.Message)
}

[pscustomobject]@{ phase = $Phase; ok = $ok; log = @($log) } | ConvertTo-Json -Compress -Depth 4
if (-not $ok) { exit 1 }
