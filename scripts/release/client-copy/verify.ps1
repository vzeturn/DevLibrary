# Runs ON the throwaway Windows machine that win-run starts (never on a real site). Installs an
# older SQL Server Express with a sample SMILE databases (no real data, Windows login only: no
# smile/smilepms password is created or changed), then walks a dl-agent build through
# install -> selftest -> upgrade -> selftest -> uninstall and writes C:\rt\out\client-copy.json.
# Placeholders replaced by client-copy.sh before upload: @OLD@ @NEW@ @CODE@ @HUB@ @SQLLINK@.
$ErrorActionPreference = 'Stop'
$out = 'C:\rt\out'; $w = 'C:\rt\work'
New-Item -ItemType Directory -Force $out, $w | Out-Null
$hub = '@HUB@'; $old = '@OLD@'; $new = '@NEW@'
$steps = New-Object System.Collections.ArrayList
function Step([string]$name, [bool]$ok, [string]$detail) {
  [void]$steps.Add([ordered]@{ step = $name; ok = $ok; detail = $detail })
  Write-Host ('[{0}] {1} {2}' -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $name, $detail)
}
function Finish {
  $steps | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $out 'client-copy.json') -Encoding UTF8
  $bad = @($steps | Where-Object { -not $_.ok }).Count
  Write-Host ("client-copy: {0} steps, {1} failed" -f $steps.Count, $bad)
  exit $(if ($bad -gt 0) { 1 } else { 0 })
}
$exe = Join-Path $env:ProgramFiles 'DevLibrary\dl-agent\dl-agent.exe'
$data = Join-Path $env:ProgramData 'dl-agent'
# Native tools write progress to stderr; under 'Stop' PS 5.1 would turn that into an error.
function Native([scriptblock]$b) { $ErrorActionPreference = 'Continue'; & $b }
function Install-Agent([string]$target, [string]$code) {
  $env:DL_AGENT_SQL_AUTH = 'windows'; $env:DL_AGENT_SQL_SERVER = '127.0.0.1'
  if ($code) { $env:DL_AGENT_ENROLL_CODE = $code } else { Remove-Item Env:DL_AGENT_ENROLL_CODE -ErrorAction SilentlyContinue }
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072
  # A child PowerShell: the installer calls exit on failure, which must not end this script.
  (New-Object Net.WebClient).DownloadFile("$hub/install.ps1", "$w\install.ps1")
  Native { powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$w\install.ps1" $target -Hub $hub 2>&1 } | Tee-Object -FilePath (Join-Path $out "install-$target.log") | Out-Host
  $rc = $LASTEXITCODE
  Remove-Item Env:DL_AGENT_ENROLL_CODE -ErrorAction SilentlyContinue
  return [int]$rc
}
function Agent-Version { if (Test-Path $exe) { ((& $exe --version 2>&1) | Out-String).Trim() } else { '' } }
function Selftest([string]$label) {
  $log = Join-Path $out ("selftest-$label.json")
  Native { & $exe selftest --json --trigger install 2>&1 } | Out-File $log -Encoding UTF8
  $rc = $LASTEXITCODE
  Step "selftest $label" ($rc -eq 0) "exit $rc, log selftest-$label.json"
}
try {
  # 1. older SQL Server Express + a sample SMILE database (SYSTEM and Administrators are sysadmin).
  Invoke-WebRequest '@SQLLINK@' -OutFile "$w\ssei.exe" -UseBasicParsing
  Start-Process "$w\ssei.exe" -ArgumentList "/ACTION=Download /MEDIAPATH=$w\sqlmedia /MEDIATYPE=Core /QUIET" -Wait
  $media = Get-ChildItem "$w\sqlmedia" -Filter *.exe | Select-Object -First 1
  Start-Process $media.FullName -ArgumentList "/q /x:$w\sqlsetup" -Wait
  $p = Start-Process "$w\sqlsetup\setup.exe" -Wait -PassThru -ArgumentList '/Q /IACCEPTSQLSERVERLICENSETERMS /ACTION=Install /FEATURES=SQLEngine /INSTANCENAME=MSSQLSERVER /SQLSYSADMINACCOUNTS="BUILTIN\Administrators" "NT AUTHORITY\SYSTEM" /TCPENABLED=1 /UPDATEENABLED=0'
  if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) { throw "SQL Server setup exit $($p.ExitCode)" }
  for ($i = 0; $i -lt 60; $i++) { if ((Get-Service MSSQLSERVER).Status -eq 'Running' -and (Test-NetConnection 127.0.0.1 -Port 1433 -InformationLevel Quiet)) { break }; Start-Sleep 5 }
  $cn = New-Object System.Data.SqlClient.SqlConnection 'Server=127.0.0.1;Integrated Security=SSPI;Database=master'
  $cn.Open()
  $ver = (New-Object System.Data.SqlClient.SqlCommand 'SELECT CAST(SERVERPROPERTY(''ProductVersion'') AS nvarchar(40))', $cn).ExecuteScalar()
  foreach ($sql in @(
      'IF DB_ID(''SMILE_FO'') IS NULL CREATE DATABASE SMILE_FO',
      'IF DB_ID(''SMILE_POS'') IS NULL CREATE DATABASE SMILE_POS',
      'IF DB_ID(''SMILE_BO'') IS NULL CREATE DATABASE SMILE_BO')) {
    (New-Object System.Data.SqlClient.SqlCommand $sql, $cn).ExecuteNonQuery() | Out-Null
  }
  foreach ($db in 'SMILE_FO', 'SMILE_POS', 'SMILE_BO') {
    $q = "USE $db; IF OBJECT_ID('dbo.SITE') IS NULL CREATE TABLE dbo.SITE (id int PRIMARY KEY, name nvarchar(80)); IF NOT EXISTS (SELECT 1 FROM dbo.SITE) INSERT dbo.SITE VALUES (1, N'Sample site')"
    (New-Object System.Data.SqlClient.SqlCommand $q, $cn).ExecuteNonQuery() | Out-Null
  }
  $cn.Close()
  Step 'sample SQL + SMILE db' $true "SQL Server $ver, databases SMILE_FO/POS/BO (sample, Windows login)"

  # 2. fresh install of the old build, then its selftest.
  $rc = Install-Agent $old '@CODE@'
  $v1 = Agent-Version
  Step "install $old" (($rc -eq 0) -and ($v1 -match [regex]::Escape($old))) "exit $rc, version '$v1'"
  Selftest 'after-install'

  # 3. upgrade in place to the build under test (keeps the identity), then its selftest.
  $id1 = if (Test-Path "$data\identity*") { (Get-ChildItem $data -Filter 'identity*' | Select-Object -First 1 | Get-FileHash).Hash } else { '' }
  $rc = Install-Agent $new ''
  $v2 = Agent-Version
  $id2 = if (Test-Path "$data\identity*") { (Get-ChildItem $data -Filter 'identity*' | Select-Object -First 1 | Get-FileHash).Hash } else { '' }
  # A channel target (canary/stable) is checked as "a different, newer build than the old one".
  $upOk = if ($new -match '^\d') { $v2 -match [regex]::Escape($new) } else { ($v2 -match '\d+\.\d+\.\d+') -and ($v2 -notmatch [regex]::Escape($old)) }
  Step "upgrade to $new" (($rc -eq 0) -and $upOk) "exit $rc, version '$v2'"
  Step 'identity kept across upgrade' (($id1 -ne '') -and ($id1 -eq $id2)) 'identity file hash unchanged'
  Selftest 'after-upgrade'
  $svc = Get-Service -Name 'dl-agent*' -ErrorAction SilentlyContinue
  Step 'service running' (($svc | Where-Object { $_.Status -eq 'Running' }) -ne $null) (($svc | ForEach-Object { "$($_.Name)=$($_.Status)" }) -join ',')

  # 4. uninstall (purge) and check nothing is left.
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072
  (New-Object Net.WebClient).DownloadFile("$hub/uninstall.ps1", "$w\uninstall.ps1")
  Native { powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$w\uninstall.ps1" -Purge 2>&1 } | Tee-Object -FilePath (Join-Path $out 'uninstall.log') | Out-Host
  $urc = $LASTEXITCODE
  $left = @()
  if (Get-Service -Name 'dl-agent*' -ErrorAction SilentlyContinue) { $left += 'service' }
  if (Test-Path $exe) { $left += 'program' }
  if (Test-Path $data) { $left += 'data' }
  Step 'uninstall -Purge' (($left.Count -eq 0) -and ($urc -eq 0)) $(if ($left.Count) { 'left: ' + ($left -join ',') } else { "service, program and data removed (exit $urc)" })
} catch {
  Step 'client-copy aborted' $false ($_ | Out-String).Trim()
}
Finish
