# Report BL: bench removes old agents first + SQL part placeholders + einvoice token env

RT-0164. Branch `feat/bench-remove-legacy` (worktree `~/Projects/DevLibrary-bl`, from 79aa03f).
Not pushed, merged or deployed.

## Commits

| SHA | Subject |
|---|---|
| `17ed934` | feat(dl-agent-einvoice): enroll token from DL_AGENT_EINVOICE_TOKEN and Windows authentication |
| `2883f35` | feat(agent-hub): bench removes the older agents first and fills SQL part placeholders |
| (this report) | docs: bench legacy removal brief and report |

## 1. Hub bench `remove_legacy`

- `src/domain/bench.ts`: `remove_legacy: z.boolean().default(true)` in the bench body;
  `BenchPayload.remove_legacy?` (sealed in the payload). `src/services/bench.ts` serves
  `payload.remove_legacy ?? false`, so a bundle sealed **before** this field keeps its old promise
  (only list, never touch); every new bundle removes unless `"remove_legacy": false`.
- New `src/install/benchLegacy.ts` (276 lines): `LEGACY_SERVICE_PATTERN` (moved, unchanged),
  `SQL_SERVICE_PATTERN`, `SQL_BINARY_PATTERN` and `LEGACY_BODY` (PowerShell 2.0+ syntax), injected
  into the bench script at `__LEGACY__`. `benchScript.ts` is 450 lines.
- Order (in `Remove-Legacy`, before the SQL prompt, fixups and install):
  1. scheduled tasks matched by name/path/action against the legacy pattern (skips `\Microsoft\*` and
     anything mentioning `dl-agent`) → `Unregister-ScheduledTask`, fallback `schtasks /Delete /F`
     (also lists via `schtasks /Query /FO CSV /V` without the module). First, so a watchdog cannot
     restart a service being stopped (the MM-Arrival watchdog task matches via its action
     `-ServiceName MM.Arrival.Agent`; source SmileCheckin `SelfCarePlan.cs`);
  2. services matched by `Get-Legacy` (pattern on name/display name, never `dl-agent`, never a SQL
     Server service by name **or** binary — a named instance `MSSQL$SMILEPMS` matches the legacy
     pattern, hence the explicit exclusion): stop, wait ≤ 30 s, then kill the PID (refuses a shared
     `svchost` PID), wait 15 s more; `sc.exe delete` (exit 1072 "marked for deletion" accepted);
  3. folders from the table below plus the folder of each removed service's binary (guards: never
     `%ProgramData%`, a Program Files root, the drive root, `%windir%`, dl-agent's own data or install
     folder, or a parent of dl-agent's data; a 2-level path only when its leaf matches the pattern),
     de-duplicated, shortest first, renamed with `[IO.Directory]::Move` to
     `<dir>.bench-old-<yyyyMMddHHmmss>`. Never deleted.
- Every action is printed and added to the summary table; the record goes to
  `%ProgramData%\dl-agent\bench-removed-<ts>.json` (`services` name/display/path/start mode, `tasks`,
  `dirs` from/to) in a `finally`, so a partial run is recorded too, followed by a one-line restore hint
  (rename each folder back, run that agent's own installer).
- The first failure: `Fail` with a clear message, `install` row `skipped`, the remaining steps
  skipped; install.ps1 never runs, so the enrolment code is not spent.
- The end-of-run legacy compare (`Test-Legacy`) now runs only when `remove_legacy` is false; the
  start-of-run listing (`Show-Legacy`) likewise.

### Directory table (in `benchLegacy.ts`, one comment per entry)

`ProgramFiles` means each of `%ProgramW6432%`, `%ProgramFiles%`, `%ProgramFiles(x86)%`.

| Base | Folder | Product | Source |
|---|---|---|---|
| ProgramData | `MM-EInvoice` | E-Invoice Go agent (config.enc, outbox, logs, exe) | MM-EINVOICE `cmd/agent/main.go` defaultConfigDir, `internal/agentcollector/service_windows.go`; dl-agent-einvoice `src/module/settings.rs` |
| ProgramData | `SMILE-HL-Agent` | SMILE-HL agent config/logs | SMILE-HLAPI `go/deploy/agent-install.ps1` configDir; dl-agent-hotellink `src/config.rs` |
| ProgramFiles | `SMILE-HL-Agent` | SMILE-HL agent binary | SMILE-HLAPI `go/deploy/agent-install.ps1` -InstallDir, `agent-uninstall.ps1` |
| ProgramFiles | `MM-Arrival Agent` | Arrival agent binary, appsettings, `data\agent-identity.bin`, watchdog.ps1 | SmileCheckin `deploy/agent/Install.ps1`; dl-agent-arrival `src/agent/layout.rs` |
| ProgramData | `MM.Membership.Agent` | Membership config.dpapi, logs | MM-MEMBERSHIP `src/MM.Membership.Agent/Configuration/AgentSecureConfig.cs`, `Observability/AgentSerilogSetup.cs`; dl-agent-membership `src/config.rs` |
| ProgramData | `SmilePMS\Agent` | PMS site agent config/state | SMILE-PMS `apps/site-agent/internal/config/config.go` DefaultPath; dl-agent-pms `src/config.rs` |
| ProgramData | `SmilePms\DeviceAgent` | PMS device agent outbox | SMILE-PMS `apps/device-agent/rust/src/config/options.rs`; dl-agent-pms-device `src/config/options.rs` |
| ProgramFiles | `SmilePms\DeviceAgent` | PMS device agent binary, appsettings | SMILE-PMS `docs/runbooks/device-agent-install.md`; dl-agent-pms-device `src/module/settings.rs` |
| ProgramData | `srbackup` | SRBackup sealed agent.json | SRBackup origin/main `agent/scripts/install.ps1` configPath (read with `git show`); dl-agent-backup `src/enroll/goagent.rs` |
| ProgramFiles | `SRBackup` | SRBackup binary | SRBackup origin/main `agent/scripts/install.ps1` InstallDir |
| ProgramData | `MM\SyncAgent` | PDAOrder SyncAgent identity, SQLite queues, site config | SML-PDAOrder `apps/sql-sync-agent/Config/AgentOptions.cs`, `wwwroot/downloads/install-sync-agent.ps1` |
| ProgramFiles | `MM\SyncAgent`, `.staged`, `.previous` | SyncAgent binary and update slots | SML-PDAOrder `wwwroot/downloads/install-sync-agent.ps1` |
| (dynamic) | folder of each removed service's binary | covers operator-chosen install dirs (e.g. Membership) | `Get-ServiceDir` |

Deliberately not touched: `%ProgramData%\MM-PDA\pda.db` (PDAOrder API legacy export, not an agent),
SRBackup's `%APPDATA%\srbackup` (per-user, the bench runs as Administrator so it would be the wrong
profile) — only `%ProgramData%\srbackup` is covered.

## 2. SQL part placeholders

- `{{SQL_SERVER}}`, `{{SQL_USER}}`, `{{SQL_PASSWORD}}`, `{{SQL_AUTH}}` (`sql`|`windows`) accepted in
  step **env** only; in args they are refused (`only {{BENCH_DIR}} in args (SQL connections and their
  parts go in env)`); any other `{{SQL_*}}` is refused. `usesSqlParts(steps)` makes the script ask for
  SQL Server even when no database is named.
- `Use-Sql` fills the parts from the one prompt (`DataSource`, user, password, `sql`/`windows`; empty
  user/password for integrated). `Resolve-Sql` replaces them and throws when no SQL is connected.
- Masking: every env value already goes through `Add-Secret` (the password included); a value that is
  exactly `{{SQL_AUTH}}` is passed as plain so `sql`/`windows` is not masked into every log line.

## 3. dl-agent `einvoice-enroll`

- `--token` is optional; `resolve_token` takes argv first, else trimmed `DL_AGENT_EINVOICE_TOKEN`,
  else fails before any network call (documented in the module docs and the `--token` help).
- Windows authentication: the collector's dl-tds `Config` treats a URL without a user as integrated
  (SSPI), so it was added: `--sql-auth windows` / `MMEI_SQL_AUTH=windows` seals a DSN without login
  or password; `sql` (default) keeps the old behaviour; anything else is refused.
- Small extension needed for `{{SQL_SERVER}}`: `--sql-host` now accepts the SSMS forms the prompt
  returns (`SRV\INSTANCE` → instance, `SRV,1444` → port) via the existing `split_server_address`.
- Not changed: the interactive `--sql-prompt` path (no Windows auth choice there).

## Checks (exact commands, all on this iMac)

| Command | Result |
|---|---|
| `HEAVY_LOCK=/tmp/heavy-dl-bl ~/.claude/scripts/heavy.sh sh -c 'export PATH=$HOME/.cargo/bin:$PATH; cargo test -p dl-agent-einvoice'` | 93 lib + 1 + 3 + 1 + 4 passed, 0 failed |
| `cargo fmt --check` | clean |
| `cargo clippy -p dl-agent-einvoice --all-targets -- -D warnings` (host) | clean |
| `cargo clippy -p dl-agent-einvoice --all-targets --target x86_64-pc-windows-gnu -- -D warnings` | clean |
| `pnpm typecheck` (services/agent-hub) | clean |
| `HEAVY_LOCK=/tmp/heavy-dl-bl ~/.claude/scripts/heavy.sh pnpm -s vitest run` (whole hub, 3 s) | 12 files, 120 tests passed |

New hub tests (`test/benchLegacy.test.ts`, 10): new bundles serve `$RemoveLegacy = $true`, `false`
when asked; a bundle sealed without the field serves `$false`; rejects a SQL part in args, an unknown
part, a non-boolean `remove_legacy`; `usesSqlParts`; SQL Server service/binary exclusion. With pwsh
(7.x on macOS, Windows parts faked: services, tasks, `sc.exe`, `schtasks.exe`, stop): full removal
over a temp tree (tasks, services in order, 4 folders renamed with contents intact, dl-agent and
siblings untouched, JSON record, summary rows); a `sc.exe delete` failure stops with nothing renamed
and a partial record; the rendered script with `remove_legacy` on parses with 0 errors, contains no
backtick or `${`, and `Resolve-Sql` fills all four parts (sql and windows) and refuses without SQL.

A real bug found by these tests and fixed: in `Get-ServiceDir` a local `$exe` shadowed the script's
`$Exe` (PowerShell names are case-insensitive), so every service-binary folder was skipped.

## Not verified on real Windows

- Stopping real services, the 30 s wait and PID kill, `sc.exe delete` and its exit codes.
- `Unregister-ScheduledTask` and the `schtasks /Query /FO CSV /V` fallback parsing (column positions,
  localized Windows headers are not used — positional), on Windows PowerShell 5.1 and 2.0.
- Renaming folders still locked by a process (e.g. an open log): `Directory.Move` fails and the bench
  stops before install — safe, but the operator must close the process and rerun.
- Exact service names/display names on the test PC (the pattern is the existing one, unchanged).
- The prompt-to-`{{SQL_SERVER}}` path end to end with the real E-Invoice enroll on a named instance.

## Concerns

- `remove_legacy` defaults to `true`: an operator who reuses an old request body without the field
  will now remove agents. Intended per the owner ("remove hết trước khi cài mới"); old sealed bundles
  are unaffected.
- The dynamic service-folder rename relies on guards; a legacy service installed directly in a
  shared folder (e.g. `C:\Tools\agent.exe`) would rename `C:\Tools` (2-level path, leaf must match
  the pattern, so `C:\Tools` would not; a deeper shared folder would). Recorded in the JSON and
  reversible by rename.
