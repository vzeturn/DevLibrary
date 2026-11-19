/**
 * Check-In (arrival) piece of the bench script (RT-0164 SMILE100): the SQL Server connection the
 * old MM-Arrival agent used, offered as the saved answer so the fresh dl-agent enrol on a hotel PC
 * asks nothing new. The old agent keeps it in plain text (ACL-restricted to SYSTEM and
 * Administrators) in `<Program Files>\MM-Arrival Agent\appsettings.Production.json` under
 * `Agent.PmsConnectionString` (SmileCheckin deploy/agent/Install.ps1); `remove_legacy` renames
 * that folder to `<dir>.bench-old-<time>` before the SQL question (benchLegacy.ts), so the newest
 * renamed copy is read too.
 *
 * `Get-ArrivalLegacySql` returns the server part without the database and with the old agent's
 * authentication (Windows or SQL login), so `Read-Sql` preselects the mode and an empty password
 * keeps the old one; the operator can still change every answer. The password goes to
 * `Add-Secret` and is never printed. PowerShell 2.0 safe (no JSON cmdlets), and free of
 * backticks and dollar-brace like the rest of the script.
 */

/** The old agent's install folder under Program Files. */
export const ARRIVAL_OLD_DIR = 'MM-Arrival Agent';

/** Where the old agent's connection came from, shown instead of "dl-agent already has". */
export const ARRIVAL_SQL_SOURCE = 'The old Check-In agent used this SQL Server connection';

export const ARRIVAL_LEGACY_SQL_BODY = String.raw`
# The SQL Server connection of the old MM-Arrival agent (its appsettings.Production.json, also in the
# folder remove_legacy renamed), server part only with its authentication, or ''.
function Get-ArrivalLegacySql {
  $roots = @($env:ProgramW6432, $env:ProgramFiles, [Environment]::GetEnvironmentVariable('ProgramFiles(x86)')) | Where-Object { $_ } | Select-Object -Unique
  $dirs = @()
  foreach ($root in $roots) {
    $dirs += (Join-Path $root '${ARRIVAL_OLD_DIR}')
    $dirs += @(Get-ChildItem -Path $root -Filter '${ARRIVAL_OLD_DIR}.bench-old-*' -ErrorAction SilentlyContinue | Where-Object { $_.PSIsContainer } | Sort-Object Name -Descending | ForEach-Object { $_.FullName })
  }
  foreach ($d in $dirs) {
    $f = Join-Path $d 'appsettings.Production.json'
    if (-not (Test-Path -LiteralPath $f)) { continue }
    try {
      $m = [regex]::Match([IO.File]::ReadAllText($f), '"PmsConnectionString"\s*:\s*"((?:[^"\\]|\\.)*)"')
      if (-not $m.Success) { continue }
      $b = New-Object Data.SqlClient.SqlConnectionStringBuilder ([regex]::Unescape($m.Groups[1].Value))
      if (-not $b.DataSource) { continue }
      Add-Secret $b.Password
      [void]$b.Remove('Initial Catalog')
      $St.SqlFrom = '${ARRIVAL_SQL_SOURCE} (' + $f + ')'
      return ($b.ConnectionString + ';')
    } catch { Warn ('The old Check-In agent settings could not be read (' + $_.Exception.Message + '): ' + $f) }
  }
  return ''
}
`;
