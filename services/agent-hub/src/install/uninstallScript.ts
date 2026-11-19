/**
 * The Windows uninstaller served at `/uninstall.ps1` (RT-0164). An agent that knows
 * `uninstall --keep-files` removes itself (tells the hub, removes the watchdog task, the service,
 * the program files and the Defender exclusion; `-Purge` also deletes identity, spool and logs).
 * For an older agent, or when its program is gone, this script removes what is left itself.
 * PowerShell 2.0 safe; no backticks or dollar-brace (raw template literal).
 */
const SCRIPT = String.raw`# DevLibrary Agent uninstaller. PowerShell as Administrator:
#   irm __HUB__/uninstall.ps1 | iex
#   & ([scriptblock]::Create((irm __HUB__/uninstall.ps1))) -Purge   (also deletes identity, spool and logs)
#   PowerShell 2.0: & ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('__HUB__/uninstall.ps1')))
# The data folder (%ProgramData%\dl-agent) stays without -Purge, so a reinstall keeps the identity.
param(
  [switch]$Purge,
  [string]$Product = 'dl-agent',
  [string]$Display = 'DevLibrary Agent'
)

$DlState = @{ Exit = 0 }

function Say([string]$m) { Write-Host ('[dl-agent] ' + $m) }
function Warn([string]$m) { Write-Host ('[dl-agent] ' + $m) -ForegroundColor Yellow }

# What an older agent leaves behind: its service, the watchdog task, the program and its exclusion.
function Remove-Leftovers([string]$exe, [string]$data) {
  if (Get-Service -Name $Product -ErrorAction SilentlyContinue) {
    & sc.exe stop $Product | Out-Null
    Start-Sleep -Seconds 3
    & sc.exe delete $Product | Out-Null
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 1072) { throw ('sc.exe delete ' + $Product + ' failed with exit code ' + $LASTEXITCODE) }
    Say ('Service ' + $Product + ' removed')
  }
  $task = $Display + ' Watchdog'
  # A missing task is an error on stderr, which Stop would turn into an exception.
  $ErrorActionPreference = 'Continue'
  & schtasks.exe /Query /TN $task 2>$null | Out-Null
  $found = ($LASTEXITCODE -eq 0)
  $ErrorActionPreference = 'Stop'
  if ($found) {
    & schtasks.exe /Delete /F /TN $task | Out-Null
    if ($LASTEXITCODE -ne 0) { throw ('schtasks /Delete ' + $task + ' failed with exit code ' + $LASTEXITCODE) }
    Say ('Task ' + $task + ' removed')
  }
  foreach ($p in @(Get-Process -Name $Product -ErrorAction SilentlyContinue)) { Stop-Process -Id $p.Id -Force }
  $dir = Split-Path $exe
  if (Get-Command Remove-MpPreference -ErrorAction SilentlyContinue) {
    try { Remove-MpPreference -ExclusionPath @($dir, $data) } catch { Warn ('Windows Defender exclusion not removed: ' + $_.Exception.Message) }
  }
  foreach ($f in @($exe, ($exe + '.new'), ($exe + '.old'), ($exe + '.download'))) { if (Test-Path $f) { Remove-Item -Force $f } }
  if ((Test-Path $dir) -and @(Get-ChildItem $dir -Force).Count -eq 0) { Remove-Item -Force $dir; Say ('Deleted ' + $dir) }
  if ($Purge -and (Test-Path $data)) { Remove-Item -Recurse -Force $data; Say ('Deleted ' + $data) }
  elseif (Test-Path $data) { Say ('Kept ' + $data + ' (identity, spool, logs); -Purge deletes it') }
}

function Uninstall-Agent {
  Set-StrictMode -Version 2.0
  $ErrorActionPreference = 'Stop'
  $ProgressPreference = 'SilentlyContinue'
  try {
    $me = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
      $DlState.Exit = 77
      Write-Host '[dl-agent] FAILED: Not running as Administrator.' -ForegroundColor Red
      Write-Host '[dl-agent] Next: open PowerShell with "Run as administrator" and paste the command again.' -ForegroundColor Yellow
      return
    }
    $pf = $env:ProgramW6432
    if (-not $pf) { $pf = $env:ProgramFiles }
    $exe = Join-Path $pf ('DevLibrary\' + $Product + '\' + $Product + '.exe')
    $data = Join-Path $env:ProgramData $Product
    $modern = (Test-Path $exe) -and ((& $exe uninstall --help | Out-String) -match 'keep-files')
    if ($modern) {
      $argv = @('uninstall', '--data', $data)
      if ($Purge) { $argv += '--purge' }
      & $exe @argv
      $rc = $LASTEXITCODE
      if ($rc -ne 0) { throw ($Product + ' uninstall stopped (exit ' + $rc + '); see above.') }
    } else {
      if (Test-Path $exe) {
        # An older agent removes its service and watchdog task; what is left is removed below.
        & $exe uninstall --data $data
        if ($LASTEXITCODE -ne 0) { Warn ($Product + ' uninstall exited ' + $LASTEXITCODE + '; removing what is left.') }
      }
      Remove-Leftovers $exe $data
      Say ($Display + ' removed')
    }
  } catch {
    if ($DlState.Exit -eq 0) { $DlState.Exit = 1 }
    Write-Host ('[dl-agent] FAILED: ' + $_.Exception.Message) -ForegroundColor Red
    Write-Host '[dl-agent] Next: fix what is named above, then run the same command again.' -ForegroundColor Yellow
  }
}

Uninstall-Agent
$global:LASTEXITCODE = $DlState.Exit
$batch = (-not [Environment]::UserInteractive) -or (@([Environment]::GetCommandLineArgs() | Where-Object { $_ -match '^-noni' }).Count -gt 0)
if ($DlState.Exit -ne 0 -and $batch) { exit $DlState.Exit }
`;

/** The uninstaller with this hub's origin in its usage lines. */
export function uninstallScript(origin: string): string {
  return SCRIPT.split('__HUB__').join(origin);
}
