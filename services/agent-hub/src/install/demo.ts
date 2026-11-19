/**
 * The `-Demo` part of `install.ps1` (RT-0261), spliced in before `Install-Agent` runs. After the
 * canary install it enrols each module with its product's demo tenant, restarts the service, runs
 * `dl-agent selftest` and prints one row per module: enrol result and worst self-test status.
 *
 * Module codes come from env `DL_AGENT_DEMO_*` or `-DemoEnv`; each reaches its enrol tool only as
 * the variable that tool reads (set for that process, removed in `finally`), never on argv, never
 * on disk. Every agent output line is masked with them. `Clear-DemoEnv` removes all of them from
 * the session when the script ends, whatever happened. PowerShell 2.0 syntax; no backtick or
 * dollar-brace (raw template literal); `__DEMO_MODULES__` becomes the `$DemoTable` block
 * (`demoModulesPs` in `../domain/demo`).
 */
export const DEMO_BLOCK = String.raw`# -- demo bench (RT-0261)
#<demo>
__DEMO_MODULES__
$DemoState = @{ Secrets = @() }

# The operator's value: -DemoEnv first, then the environment; '' when unset.
function Get-DemoValue([string]$name) {
  if ($DemoEnv.ContainsKey($name)) { return [string]$DemoEnv[$name] }
  return [string][Environment]::GetEnvironmentVariable($name)
}

function Hide-Demo([string]$m) {
  foreach ($s in $DemoState.Secrets) { if ($s -and $s.Length -ge 4) { $m = $m.Replace($s, '***') } }
  return $m
}

# The modules to set up, in table order (all of them unless -DemoModules names some).
function Get-DemoRows {
  $out = @()
  foreach ($row in $DemoTable) {
    if ($DemoModules.Count -eq 0 -or ($DemoModules -contains $row.Module)) { $out += $row }
  }
  return ,$out
}

# Worst status per module in 'dl-agent selftest' table lines: fail > warn > pass > skip.
function Get-DemoStatus([string[]]$lines) {
  $rank = @{ skip = 0; pass = 1; warn = 2; fail = 3 }
  $worst = @{}
  foreach ($l in $lines) {
    $m = [regex]::Match([string]$l, '^\s*(pass|warn|fail|skip)\s+(\S+)')
    if (-not $m.Success) { continue }
    $st = $m.Groups[1].Value
    $mod = $m.Groups[2].Value
    if (-not $worst.ContainsKey($mod) -or $rank[$st] -gt $rank[$worst[$mod]]) { $worst[$mod] = $st }
  }
  return $worst
}

# One module's enrol tool. Returns @(result, note); result is ok, skip, n/a or FAILED.
function Invoke-DemoStep($row, [string]$exe, [string]$data) {
  if (-not $row.Setup) { return @('n/a', [string]$row.Note) }
  $missing = @()
  foreach ($n in (@($row.SecretNeed) + @($row.OptionNeed))) { if (-not (Get-DemoValue $n)) { $missing += $n } }
  if ($missing.Count -gt 0) { return @('skip', ('set ' + ($missing -join ', '))) }
  $argv = @([string]$row.Setup)
  $from = @($row.OptionFrom)
  $flag = @($row.OptionFlag)
  for ($i = 0; $i -lt $from.Count; $i++) {
    $v = Get-DemoValue ([string]$from[$i])
    if ($v) { $argv += @([string]$flag[$i], $v) }
  }
  $argv += @('--data', $data)
  $set = @()
  try {
    $sFrom = @($row.SecretFrom)
    $sTo = @($row.SecretTo)
    for ($i = 0; $i -lt $sFrom.Count; $i++) {
      $v = Get-DemoValue ([string]$sFrom[$i])
      if (-not $v) { continue }
      $DemoState.Secrets += $v
      [Environment]::SetEnvironmentVariable([string]$sTo[$i], $v, 'Process')
      $set += [string]$sTo[$i]
    }
    # Options are URLs and ids, never secrets: the line shows what runs.
    Say ('Demo: dl-agent ' + ($argv -join ' '))
    $rc = -1
    $eap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
      & $exe @argv 2>&1 | ForEach-Object { Write-Host ('    ' + (Hide-Demo ([string]$_))) }
      $rc = $LASTEXITCODE
    } finally { $ErrorActionPreference = $eap }
    if ($rc -eq 0) { return @('ok', [string]$row.Note) }
    return @('FAILED', ('exit code ' + $rc))
  } finally {
    foreach ($n in $set) { [Environment]::SetEnvironmentVariable($n, $null, 'Process') }
  }
}

# Restarts the service so the modules read what the enrol tools wrote; waits up to 90 s for the hub.
function Restart-DemoAgent([string]$data) {
  $beat = Join-Path $data 'heartbeat.json'
  $since = (UnixNow) - 5
  try { Restart-Service -Name $Product -Force -ErrorAction Stop }
  catch { Warn ('Service restart failed: ' + $_.Exception.Message); return 'FAILED' }
  $deadline = (UnixNow) + 90
  while ((UnixNow) -lt $deadline) {
    if (Test-Path $beat) {
      $m = [regex]::Match([IO.File]::ReadAllText($beat), '"hub_at"\s*:\s*(\d+)')
      if ($m.Success -and ([long]$m.Groups[1].Value) -ge $since) { return 'ok' }
    }
    Start-Sleep -Seconds 3
  }
  Warn 'No hub contact within 90 s of the restart; the self test runs anyway.'
  return 'warn'
}

function New-DemoRow([string]$module, [string]$enrol, [string]$note) {
  return (New-Object PSObject -Property @{ Module = $module; Enrol = $enrol; Selftest = ''; Note = (Hide-Demo $note) })
}

function Invoke-Demo {
  Set-StrictMode -Version 2.0
  $ErrorActionPreference = 'Stop'
  $pf = $env:ProgramW6432
  if (-not $pf) { $pf = $env:ProgramFiles }
  $exe = Join-Path $pf ('DevLibrary\' + $Product + '\' + $Product + '.exe')
  $data = Join-Path $env:ProgramData $Product
  Say 'Demo bench: enrolling each module with its demo tenant.'
  $rows = @()
  foreach ($row in (Get-DemoRows)) {
    try { $r = Invoke-DemoStep $row $exe $data }
    catch { $r = @('FAILED', $_.Exception.Message) }
    if ($r[0] -eq 'FAILED') { Warn ('Demo: ' + $row.Module + ' did not enrol; going on with the next module.') }
    $rows += New-DemoRow ([string]$row.Module) ([string]$r[0]) ([string]$r[1])
  }
  $core = New-DemoRow 'core' (Restart-DemoAgent $data) 'service restart and hub contact'
  $rows = @($core) + $rows

  $lines = @()
  $eap = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try {
    if ((& $exe --help 2>&1 | Out-String) -match '\bselftest\b') {
      Say 'Self test (dl-agent selftest --trigger install); the agent uploads it to the hub.'
      $lines = @(& $exe selftest --trigger install --data $data 2>&1 | ForEach-Object { [string]$_ })
    } else { Warn 'This dl-agent build has no selftest command: self test skipped.' }
  } finally { $ErrorActionPreference = $eap }
  foreach ($l in $lines) { Write-Host ('    ' + (Hide-Demo $l)) }
  $status = Get-DemoStatus $lines
  foreach ($r in $rows) {
    if ($status.ContainsKey($r.Module)) { $r.Selftest = $status[$r.Module] } else { $r.Selftest = 'none' }
  }

  Write-Host ''
  Write-Host (($rows | Format-Table -Property Module, Enrol, Selftest, Note -AutoSize | Out-String).TrimEnd())
  $bad = @($rows | Where-Object { $_.Enrol -eq 'FAILED' -or $_.Selftest -eq 'fail' }).Count
  $warn = @($rows | Where-Object { $_.Enrol -eq 'skip' -or $_.Enrol -eq 'warn' -or $_.Selftest -eq 'warn' }).Count
  if ($bad -gt 0) {
    Write-Host ('[dl-agent] Demo: ' + $bad + ' module(s) failed, ' + $warn + ' to look at.') -ForegroundColor Red
    $DlState.Exit = 1
  } elseif ($warn -gt 0) {
    Write-Host ('[dl-agent] Demo: no failure, ' + $warn + ' module(s) to look at.') -ForegroundColor Yellow
  } else {
    Write-Host '[dl-agent] Demo: every module passed.' -ForegroundColor Green
  }
}

# Removes every demo variable (operator and tool side) from this session and empties -DemoEnv.
function Clear-DemoEnv {
  foreach ($row in $DemoTable) {
    foreach ($n in (@($row.SecretFrom) + @($row.SecretTo) + @($row.OptionFrom))) {
      if ($n) { Remove-Item -Path ('Env:' + $n) -ErrorAction SilentlyContinue }
    }
  }
  foreach ($n in @('DL_AGENT_DEMO', 'DL_AGENT_DEMO_MODULES')) { Remove-Item -Path ('Env:' + $n) -ErrorAction SilentlyContinue }
  $DemoEnv.Clear()
  $DemoState.Secrets = @()
}
#</demo>`;
