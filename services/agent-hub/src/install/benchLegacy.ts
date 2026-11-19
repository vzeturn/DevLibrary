/**
 * The part of the bench script (RT-0164) that deals with the older per-product agents: lists them
 * (`remove_legacy: false`) or removes them before anything is installed (`remove_legacy: true`):
 * their scheduled tasks are unregistered, their services stopped and deleted, and their data,
 * identity and install folders renamed to `<dir>.bench-old-<yyyyMMddHHmmss>` (never deleted).
 * SQL Server services, `dl-agent` and anything not matched are never touched.
 *
 * Same rules as the rest of the script: Windows PowerShell 2.0 syntax, no backticks and no
 * dollar-brace, so it lives in a raw template literal.
 */

/**
 * Windows services of the per-product agents dl-agent takes over (E-Invoice, SmilePmsAgent,
 * SmilePmsDeviceAgent, SmileHLAgent, MM.Arrival.Agent, MM.Membership.Agent, SRBackup,
 * PdaSyncAgent), matched case-insensitively against name, display name and path; scheduled tasks
 * are matched against their path, name and action.
 */
export const LEGACY_SERVICE_PATTERN =
  'mmei|e-?invoice|smile ?pms|smile ?hl|mm\\.arrival|mm\\.membership|arrival\\.agent|membership\\.agent|srbackup|pda ?sync';

/** SQL Server's own services, by name: never touched even when a named instance matches the pattern (MSSQL$SMILEPMS). */
export const SQL_SERVICE_PATTERN =
  '^(mssql|sqlagent|sqlserveragent|sqlbrowser|sqlwriter|sqltelemetry|ssistelemetry|ssastelemetry|msolap|mssqlserverolapservice|mssqlfdlauncher|reportserver|sqlserverreportingservices|msdtsserver)';
/** ... and by binary. */
export const SQL_BINARY_PATTERN = '\\\\microsoft sql server\\\\|sqlservr\\.exe|sqlagent\\.exe|sqlbrowser\\.exe|sqlwriter\\.exe';

export const LEGACY_BODY = String.raw`
# Data, identity and install folders of the older agents, renamed (never deleted) by Remove-Legacy.
# Base: ProgramData = %ProgramData%; ProgramFiles = both the 64-bit and the x86 Program Files.
# Each entry cites where the product's own installer or the dl-agent adoption code puts it.
$LegacyDirs = @(
  # E-Invoice Go agent: config.enc, outbox, logs and mmei-agent.exe (MM-EINVOICE cmd/agent/main.go defaultConfigDir,
  # internal/agentcollector/service_windows.go; dl-agent-einvoice src/module/settings.rs default_config_dir)
  @{ Base = 'ProgramData'; Path = 'MM-EInvoice' },
  # SMILE-HL agent config.json and logs (SMILE-HLAPI go/deploy/agent-install.ps1 configDir; dl-agent-hotellink src/config.rs)
  @{ Base = 'ProgramData'; Path = 'SMILE-HL-Agent' },
  # SMILE-HL agent binary (SMILE-HLAPI go/deploy/agent-install.ps1 -InstallDir, agent-uninstall.ps1)
  @{ Base = 'ProgramFiles'; Path = 'SMILE-HL-Agent' },
  # MM-Arrival agent: binary, appsettings.Production.json, data\agent-identity.bin, watchdog.ps1
  # (SmileCheckin deploy/agent/Install.ps1 InstallDir/dataDir; dl-agent-arrival src/agent/layout.rs)
  @{ Base = 'ProgramFiles'; Path = 'MM-Arrival Agent' },
  # MM.Membership agent config.dpapi and logs (MM-MEMBERSHIP src/MM.Membership.Agent/Configuration/AgentSecureConfig.cs,
  # Observability/AgentSerilogSetup.cs; dl-agent-membership src/config.rs)
  @{ Base = 'ProgramData'; Path = 'MM.Membership.Agent' },
  # SmilePMS site agent config.json and state (SMILE-PMS apps/site-agent/internal/config/config.go DefaultPath; dl-agent-pms src/config.rs)
  @{ Base = 'ProgramData'; Path = 'SmilePMS\Agent' },
  # SmilePms device agent result outbox (SMILE-PMS apps/device-agent/rust/src/config/options.rs DataDirectory;
  # dl-agent-pms-device src/config/options.rs)
  @{ Base = 'ProgramData'; Path = 'SmilePms\DeviceAgent' },
  # SmilePms device agent binary and appsettings.json (SMILE-PMS docs/runbooks/device-agent-install.md;
  # dl-agent-pms-device src/module/settings.rs)
  @{ Base = 'ProgramFiles'; Path = 'SmilePms\DeviceAgent' },
  # SRBackup Go agent sealed agent.json (SRBackup origin/main agent/scripts/install.ps1 configPath;
  # dl-agent-backup src/enroll/goagent.rs candidates)
  @{ Base = 'ProgramData'; Path = 'srbackup' },
  # SRBackup agent binary (SRBackup origin/main agent/scripts/install.ps1 InstallDir)
  @{ Base = 'ProgramFiles'; Path = 'SRBackup' },
  # PDAOrder SyncAgent identity, SQLite queues, appsettings.site.json and update folder
  # (SML-PDAOrder apps/sql-sync-agent/Config/AgentOptions.cs, wwwroot/downloads/install-sync-agent.ps1)
  @{ Base = 'ProgramData'; Path = 'MM\SyncAgent' },
  # PDAOrder SyncAgent binary and its update slots (SML-PDAOrder wwwroot/downloads/install-sync-agent.ps1 InstallDir, .staged, .previous)
  @{ Base = 'ProgramFiles'; Path = 'MM\SyncAgent' },
  @{ Base = 'ProgramFiles'; Path = 'MM\SyncAgent.staged' },
  @{ Base = 'ProgramFiles'; Path = 'MM\SyncAgent.previous' }
)

function Get-Services {
  if (Get-Command Get-CimInstance -ErrorAction SilentlyContinue) { return @(Get-CimInstance -ClassName Win32_Service -ErrorAction Stop) }
  return @(Get-WmiObject -Class Win32_Service -ErrorAction Stop)
}
function Test-SqlService($svc) {
  return (([string]$svc.Name -match $SqlServicePattern) -or ([string]$svc.PathName -match $SqlBinaryPattern))
}
function Get-Legacy {
  @(Get-Services | Where-Object { $_.Name -ne $Product -and -not (Test-SqlService $_) -and (($_.Name + ' ' + $_.DisplayName + ' ' + $_.PathName) -match $LegacyPattern) } | Sort-Object Name)
}

# remove_legacy false: read only, listed before anything changes and checked again at the end.
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

# Scheduled tasks of the older agents (watchdogs, updaters) that could restart or reinstall them.
function Test-LegacyTask([string]$path, [string]$text) {
  return ($path -notlike '\Microsoft\*' -and $text -notmatch 'dl-agent' -and $text -match $LegacyPattern)
}
function Get-LegacyTasks {
  $found = New-Object Collections.ArrayList
  if (Get-Command Get-ScheduledTask -ErrorAction SilentlyContinue) {
    foreach ($t in @(Get-ScheduledTask -ErrorAction Stop)) {
      $acts = (@($t.Actions | ForEach-Object { ([string]$_.Execute + ' ' + [string]$_.Arguments).Trim() }) -join ' ; ')
      if (Test-LegacyTask ([string]$t.TaskPath) ([string]$t.TaskPath + [string]$t.TaskName + ' ' + $acts)) {
        [void]$found.Add((New-Object PSObject -Property @{ Path = [string]$t.TaskPath; Name = [string]$t.TaskName; Action = $acts }))
      }
    }
    return $found
  }
  # Windows 7 / 2008 R2: schtasks, read by column position (the headers are localized).
  $hdr = @('Host', 'Full')
  for ($i = 3; $i -le 40; $i++) { if ($i -eq 9) { $hdr += 'Run' } else { $hdr += ('C' + $i) } }
  $seen = @{}
  foreach ($r in @(& schtasks.exe /Query /FO CSV /V /NH | ConvertFrom-Csv -Header $hdr)) {
    $full = [string]$r.Full
    if (-not $full -or $seen.ContainsKey($full)) { continue }
    $seen[$full] = $true
    $i = $full.LastIndexOf('\')
    $path = $full.Substring(0, $i + 1)
    if (Test-LegacyTask $path ($full + ' ' + [string]$r.Run)) {
      [void]$found.Add((New-Object PSObject -Property @{ Path = $path; Name = $full.Substring($i + 1); Action = [string]$r.Run }))
    }
  }
  return $found
}
function Remove-LegacyTask($t) {
  $full = $t.Path + $t.Name
  if (Get-Command Unregister-ScheduledTask -ErrorAction SilentlyContinue) {
    try { Unregister-ScheduledTask -TaskName $t.Name -TaskPath $t.Path -Confirm:$false -ErrorAction Stop; return }
    catch { Warn ('Unregister-ScheduledTask ' + $full + ' failed (' + $_.Exception.Message + '); trying schtasks.') }
  }
  & schtasks.exe /Delete /TN $full /F | Out-Null
  if ($LASTEXITCODE -ne 0) { throw ('schtasks /Delete ' + $full + ' failed with exit code ' + $LASTEXITCODE) }
}

# Stops a service: 30 s for a clean stop, then its process is killed by PID.
function Stop-LegacyService([string]$name) {
  $s = Get-Service -Name $name -ErrorAction Stop
  if ($s.Status -eq 'Stopped') { return 'was stopped' }
  try { if ($s.CanStop) { $s.Stop() } }
  catch { Warn ('Stop request for ' + $name + ' failed: ' + $_.Exception.Message) }
  try { $s.WaitForStatus('Stopped', (New-TimeSpan -Seconds 30)); return 'stopped' }
  catch { Warn ('Service ' + $name + ' did not stop within 30 s; killing its process.') }
  $p = 0
  foreach ($w in @(Get-Services)) { if ([string]$w.Name -eq $name) { $p = [int]$w.ProcessId } }
  if ($p -gt 4) {
    $proc = Get-Process -Id $p -ErrorAction Stop
    if ($proc.ProcessName -eq 'svchost') { throw ('service ' + $name + ' runs inside svchost (PID ' + $p + '): not killed') }
    Stop-Process -Id $p -Force -ErrorAction Stop
  }
  $s.Refresh()
  try { $s.WaitForStatus('Stopped', (New-TimeSpan -Seconds 15)) }
  catch { throw ('service ' + $name + ' is still ' + $s.Status + ' after its process (PID ' + $p + ') was killed') }
  return ('killed PID ' + $p)
}

function Get-ProgramFilesDirs {
  $pf = @()
  foreach ($d in @($env:ProgramW6432, $env:ProgramFiles, [Environment]::GetEnvironmentVariable('ProgramFiles(x86)'))) {
    if ($d -and ($pf -notcontains $d)) { $pf += $d }
  }
  return $pf
}
# The folder of a service binary, unless it is a system or shared folder.
function Get-ServiceDir([string]$pathName) {
  $m = [regex]::Match($pathName, '^\s*"([^"]+)"|^\s*(\S.*?\.exe)\b', 'IgnoreCase')
  if (-not $m.Success) { return $null }
  # Not $exe: PowerShell names are case-insensitive and $Exe is dl-agent's own binary.
  $bin = $m.Groups[1].Value
  if (-not $bin) { $bin = $m.Groups[2].Value }
  $dir = [IO.Path]::GetDirectoryName([Environment]::ExpandEnvironmentVariables($bin))
  if (-not $dir) { return $null }
  $dir = $dir.TrimEnd('\')
  $keep = @($env:ProgramData, $env:SystemDrive, (Join-Path $pf 'DevLibrary')) + (Get-ProgramFilesDirs)
  foreach ($k in $keep) { if ($k -and $dir -ieq $k.TrimEnd('\')) { return $null } }
  foreach ($k in @($env:windir, $Data, (Split-Path $Exe))) { if ($k -and ($dir + '\').StartsWith($k.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) { return $null } }
  if (($Data + '\').StartsWith($dir + '\', [StringComparison]::OrdinalIgnoreCase)) { return $null }
  if (($dir.Split('\').Count -lt 3) -and ((Split-Path $dir -Leaf) -notmatch $LegacyPattern)) { return $null }
  return $dir
}
function Get-LegacyDirs([object[]]$services) {
  $all = @()
  foreach ($e in $LegacyDirs) {
    if ($e.Base -eq 'ProgramData') { $bases = @($env:ProgramData) } else { $bases = Get-ProgramFilesDirs }
    foreach ($b in $bases) { if ($b) { $all += (Join-Path $b $e.Path) } }
  }
  foreach ($svc in $services) { $d = Get-ServiceDir ([string]$svc.PathName); if ($d) { $all += $d } }
  $seen = @{}
  $out = @()
  # Shortest first: a parent moves before its children, which then go with it.
  foreach ($d in @($all | Sort-Object Length)) {
    $k = $d.ToLowerInvariant()
    if (-not $seen.ContainsKey($k)) { $seen[$k] = $true; $out += $d }
  }
  return $out
}

function Json([string]$s) {
  $b = New-Object Text.StringBuilder
  foreach ($c in $s.ToCharArray()) {
    if ($c -eq '"' -or $c -eq '\') { [void]$b.Append('\').Append($c) }
    elseif ([int]$c -lt 32) { [void]$b.Append('\u').Append(([int]$c).ToString('x4')) }
    else { [void]$b.Append($c) }
  }
  return '"' + $b.ToString() + '"'
}
function Save-Removed($done, [string]$file) {
  $svc = @($done.Services | ForEach-Object { '{"name":' + (Json $_.Name) + ',"display_name":' + (Json $_.DisplayName) + ',"path":' + (Json $_.PathName) + ',"start_mode":' + (Json $_.StartMode) + '}' })
  $tasks = @($done.Tasks | ForEach-Object { '{"path":' + (Json $_.Path) + ',"name":' + (Json $_.Name) + ',"action":' + (Json $_.Action) + '}' })
  $dirs = @($done.Dirs | ForEach-Object { '{"from":' + (Json $_.From) + ',"to":' + (Json $_.To) + '}' })
  $text = '{"removed_at":' + (Json $done.At) + ',"services":[' + ($svc -join ',') + '],"tasks":[' + ($tasks -join ',') + '],"dirs":[' + ($dirs -join ',') + ']}'
  [IO.File]::WriteAllText($file, $text, (New-Object Text.UTF8Encoding $false))
}

# remove_legacy true: tasks first (a watchdog would restart a stopped service), then services,
# then folders. The first failure stops it; what was done so far is recorded either way.
function Remove-Legacy {
  $ts = Get-Date -Format 'yyyyMMddHHmmss'
  $done = @{ At = (Get-Date).ToString('o'); Services = @(); Tasks = @(); Dirs = @() }
  $file = Join-Path $Data ('bench-removed-' + $ts + '.json')
  $step = 'older agents'
  try {
    $services = @(Get-Legacy)
    $tasks = @(Get-LegacyTasks)
    $dirs = @(Get-LegacyDirs $services | Where-Object { Test-Path -LiteralPath $_ -PathType Container })
    if ($services.Count + $tasks.Count + $dirs.Count -eq 0) { Say 'No older per-product agent on this PC.'; Add-Result $step 'ok' 'none found'; return $true }
    Say ('Removing the older per-product agents (' + $services.Count + ' service(s), ' + $tasks.Count + ' task(s), ' + $dirs.Count + ' folder(s)); folders are renamed, not deleted.')
    foreach ($t in $tasks) {
      $step = 'remove task ' + $t.Path + $t.Name
      Remove-LegacyTask $t
      $done.Tasks += $t
      Say ('Task ' + $t.Path + $t.Name + ' unregistered (' + $t.Action + ').')
      Add-Result $step 'ok' $t.Action
    }
    foreach ($svc in $services) {
      $name = [string]$svc.Name
      $step = 'remove service ' + $name
      $how = Stop-LegacyService $name
      & sc.exe delete $name | Out-Null
      if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 1072) { throw ('sc.exe delete ' + $name + ' failed with exit code ' + $LASTEXITCODE) }
      $done.Services += $svc
      Say ('Service ' + $name + ' (' + $svc.DisplayName + '): ' + $how + ', deleted.')
      Add-Result $step 'ok' ($how + ', deleted')
    }
    foreach ($d in $dirs) {
      if (-not (Test-Path -LiteralPath $d -PathType Container)) { continue }
      $step = 'rename ' + $d
      $new = $d + '.bench-old-' + $ts
      [IO.Directory]::Move($d, $new)
      $done.Dirs += (New-Object PSObject -Property @{ From = $d; To = $new })
      Say ('Folder ' + $d + ' -> ' + $new)
      Add-Result $step 'ok' ('-> ' + (Split-Path $new -Leaf))
    }
    return $true
  } catch {
    Fail ('Removing the older agents failed at "' + $step + '": ' + $_.Exception.Message)
    Add-Result $step 'FAILED' $_.Exception.Message
    return $false
  } finally {
    if ($done.Services.Count + $done.Tasks.Count + $done.Dirs.Count -gt 0) {
      try {
        Save-Removed $done $file
        Say ('What was removed is listed in ' + $file + '. To restore: rename each folder back to its "from" name, then run that agent''s own installer to register its service again.')
      } catch { Warn ('Could not write ' + $file + ': ' + $_.Exception.Message + '; the actions are in the log above.') }
    }
  }
}
`;
