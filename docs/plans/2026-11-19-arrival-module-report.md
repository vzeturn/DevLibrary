# arrival module: report

Brief: `docs/plans/2026-11-19-arrival-module-brief.md` (RT-0164, task 557 step 5, slice A1).
Branch `feat/dl-agent-arrival` from main `fc49a0a`. Not pushed. SmileCheckin was only read.

## What changed

New crate `crates/dl-agent-arrival` (package `dl-agent-arrival`, module `arrival`), about 11,300
lines of Rust including tests, no third-party crates, no `unsafe`.

- `src/contracts/`: port of `MM.Arrival.Sync.Contracts`. `envelope.rs` (`SyncEnvelope`:
  AES-256-GCM, HKDF-SHA256 per direction and message type, ±120 s window), `messages.rs`
  (`SyncMessages`), `hasher.rs` (`CanonicalHasher`), `delta.rs` (`ResultSetDelta`),
  `projection.rs` (`PmsProjection`), `validator.rs` + `values.rs` + `xml.rs`
  (`PmsRequestValidator` with the .NET parsers' leniencies and an `XmlReader` stand-in),
  `catalog.rs` (`PmsOperationCatalog`, loaded from `assets/catalog.json`), `key_vault.rs`
  (`TenantKeyVault`, `AgentKeyOps`), `vault_ops.rs` (`LegalVaultOps`), `row.rs`, and
  `contracts::errors` (`PmsErrors`: only the SQL error number crosses the wire).
- `src/stj.rs`, `src/stj/num.rs`, `src/netdec.rs`: JSON text exactly as System.Text.Json writes it
  (escaping, shortest round-trip doubles, `DateTimeOffset`), and .NET `decimal` parsing and text.
- `src/crypto/`: AES-256 (forward only) and GCM, HKDF on `dl-hash`; checked against vectors .NET
  produced.
- `src/agent/`: port of `MM.Arrival.Agent`. `runtime.rs` (`CloudLinkWorker` loop, lanes, uplinks,
  stop), `cloud.rs` (one sealed exchange), `outbox.rs` (uplink queues), `processor.rs`
  (`RequestProcessor`), `executor.rs` + `tds.rs` + `sqlvalue.rs` (`IPmsExecutor` /
  `SqlPmsExecutor` over `dl-tds`, `PmsConnectionProbe`), `installer.rs`
  (`PmsProcedureInstaller`, `assets/pms-procedures.sql` byte for byte), `watcher.rs`
  (`ChangeWatcher`), `snapshots.rs` (`SnapshotStore`), `journal.rs` (`IdempotencyLog`),
  `link_status.rs` (`LinkStatusFile` + heartbeat), `vault.rs` (`LegalVault`), `secrets.rs`
  (identity and tenant key, DPAPI LocalMachine with the .NET entropies through
  `dl_winsvc::dpapi::*_with_entropy`), `settings.rs` (`AgentOptions` read from both appsettings
  files with .NET configuration binding rules), `layout.rs` (install dir and relative paths),
  `health.rs` (state, metrics, rate-limited reports), `clock.rs` (test seam for skew).
- `src/module/`: `ArrivalModule` (opt-in), `arrival_module() -> Factory`,
  `arrival_module_with(Hooks)` for tests; `reporter.rs` routes state, metrics and reports to the
  module context.
- `src/tools/enroll.rs`: `dl-agent arrival-enroll`.
- `assets/`: `catalog.json` (88 operations, exported from the .NET catalog), `pms-procedures.sql`
  (the .NET bundle), `appsettings.json` (the .NET base file, written by enroll when absent).
- `testdata/golden.json` (outputs of the .NET code: AES-GCM, HKDF, envelopes, 542 STJ values, 458
  canonical numbers, row and parameter hashes, 45 projections, 6 deltas, 170 parameter values, 14
  requests, errors, vault paths, tenant key, identity, link status, bundle hashes) and
  `testdata/pms-fake.sql` (copy of SmileCheckin `deploy/dev/pms-fake.sql`). The golden data and the
  catalog came from a throwaway .NET exporter in the session scratch directory, not committed.

Elsewhere:

- `crates/dl-agent-bin`: `dl-agent-arrival.workspace = true`; `main.rs` registers
  `.module(dl_agent_arrival::arrival_module()).tool(dl_agent_arrival::arrival_enroll_tool())`
  after `pms_device_module()`. `main.rs` has no `use` lines (it calls with full paths), so none
  was added.
- Workspace `Cargo.toml`: the `dl-agent-arrival` path dependency. `.cargo/config.toml`: the crate
  is left out of `cargo wasm` (sockets, SQL, DPAPI).
- `CHANGELOG.md` (Unreleased / Added), `crates/dl-agent/README.md` (module table),
  `crates/dl-agent-arrival/README.md`.

## Placement decision: read the install in place

The module reads `appsettings*.json`, `agent-identity.bin`, `agent-tenant-key.bin`, `vault/`,
`agent-idempotency.jsonl` and writes `link-status.json` / `link-heartbeat.txt` where the .NET
install keeps them (`modules.arrival.install_dir`, default `%ProgramW6432%\MM-Arrival Agent`, or the
one other `MM-Arrival Agent` folder under Program Files that holds `data\agent-identity.bin`). It
does not move them into `ctx.data_dir()/arrival`, because:

- No re-enrollment and no data step at switch-over: the three production hotels change service,
  not state. Nothing can be lost half-way through a move (the vault holds legal documents the hotel
  must keep; the journal stops double check-ins after a restart).
- Rollback stays trivial: the .NET service can be started again on the same files during the
  canary (aurelia-demo) and the first hotels. After a move the old service would see an empty
  install.
- The files are already protected the way the module needs (DPAPI LocalMachine + entropy, the
  ACL Install.ps1 sets on the folder); a copy would need the same protection set up twice.
- The tray and Install.ps1 read `link-status.json` there.

Relative paths in the settings resolve as the .NET service saw them (working directory
`%SystemRoot%\System32`: an existing file there first, then under the install directory). Off
Windows the install directory defaults to the module's data directory. Once the .NET service is
retired for good, a later slice can move the files; the module would only need `install_dir`.

## Differences from .NET, and why

- **HTTP/1.1, not HTTP/2.** `dl-http` speaks HTTP/1.1; the long poll and results posts need no
  multiplexing. Plain `http://` only to loopback hosts (tests); anything else must be https.
- **Procedure bundle: .NET semantics, not dl-agent's signed proc-bundle.** The bundle is compiled
  in and installed exactly like `PmsProcedureInstaller` (same batch split, the same extended
  property markers and hashes, SMILE_PICTURE phase on `master` first, pending marker first), so a
  site the .NET agent installed is recognised as current and not reinstalled, and a switch back
  to .NET also sees its own marker. The signed bundle machinery would add a second marker and a
  signature the .NET side does not know; it can replace this once .NET is gone.
- **Catalog and bundle reach sites with a dl-agent release.** Both are compiled in
  (`include_str!`); there is no runtime download. The catalog is the security boundary, so it only
  changes through a reviewed release.
- **TCVN3 stays in SQL.** The .NET agent converts no text: the procedures call
  `dbo.fTCVNToUnicode`. Converting again with `dl-vn` would double-convert, so `dl-vn` is not used.
- **SQL through `dl-tds`, parameters only.** An ADO string without an `Encrypt` key gets
  `Encrypt=true` (Microsoft.Data.SqlClient's default, certificate checked); `TrustServerCertificate`
  is never added. Strings and dates map to `nvarchar` and `datetime2` parameters as SqlClient does.
  A pool login failure is reported as PMS unavailable (`arrival_pms_failed`), never with the
  connection string. The pool holds `MaxConcurrency + watches + 1` sessions.
- **Dropped:** `Update/`, `SelfCare/`, `Diagnostics/` (ProblemJournal, Event Log history), the
  service host. dl-agent owns updates and problem reports. The hello carries no problems, no
  inventory and no `UpdateCheck`; its `agentVersion` is `dl-agent/<version>`.
- **Settings:** `Agent__*` environment overrides are not read (a Windows service of dl-agent does
  not carry the old service's environment). Settings changes take effect at the next module start.
- **Journal:** an append that fails is logged and the answer is still sent `Ok` (the PMS write
  already happened; failing the answer would make the cloud retry a write that succeeded). The
  journal file is created on the first append, as in .NET.
- **Stop:** queued requests not yet started are dropped (the cloud asks again); running ones get
  5 s (`GRACE`); the journal is flushed. A dropped uplink batch reports `arrival_cloud_failed`.
- **Not enrolled / bad settings:** `degraded` with the reason, re-checked every 60 s, no restart
  loop; an invalid `modules.arrival` section stays degraded until the config changes. The watcher
  feeds PMS health, so a PMS outage shows without a cloud request.
- **No module commands** (answered `unknown command`): status lives in the module state and metrics.
- **Enroll** writes `appsettings.Production.json` (`CloudBaseUrl`, `PmsConnectionString`,
  `IdentityFile`, `IdempotencyFile`) and `data\agent-identity.bin` like Install.ps1, owner-only
  (0600/0700, ACL on Windows), without a BOM and without `ServiceName` / `AutoUpdate` (dl-agent's
  job). It writes the base `appsettings.json` only when absent, prints the tenant key fingerprint
  (never the key), and on `--force` leaves an identity at an older path as it was (the settings
  name the new one).
- **Byte-compat fix found by the golden tests:** typed `DateTimeOffset` properties (`atUtc` in
  `link-status.json`, `at` in the journal) are written raw (`+00:00`), while JSON string values
  keep the default encoder's `+`. Row values stay escaped, as the golden rows show.

## Brief checklist

| Item | State |
|---|---|
| Crate, `arrival_module() -> Factory`, opt-in, registered in dl-agent-bin | Done |
| Read in place: appsettings, identity, tenant key, vault, journal | Done (DPAPI + entropy via dl-winsvc; plain JSON off Windows) |
| Module config `enabled`, `install_dir` (+ `utc_offset_minutes`, default 420) | Done |
| Port of contracts + agent, catalog as JSON with a load test | Done (`catalog_loads_with_every_procedure_in_the_bundle`) |
| Byte compatibility with .NET golden fixtures | Done (contracts, crypto, identity, link status, journal, bundle) |
| `arrival-enroll` with env/stdin secrets, `--force`, probe / `--no-probe` | Done |
| Degraded states, running after first poll, prompt stop with grace and flush | Done |
| Metrics and the six report codes, rate-limited, no guest data / SQL / keys | Done |
| Fake cloud: poll, replay, skew, 401 | Done (+ unreachable cloud) |
| SQL integration, env-gated | Done, run once against SQL Server 2022 (below) |
| Module lifecycle: disabled, not enrolled, prompt stop | Done (+ bad config, enrollment picked up) |

## Checks (run in the worktree)

| Command | Result |
|---|---|
| `~/.cargo/bin/cargo fmt --all` (then `-- --check`) | clean |
| `~/.cargo/bin/cargo test -p dl-agent-arrival -p dl-agent-bin` | 77 passed, 0 failed |
| `~/.cargo/bin/cargo clippy -p dl-agent-arrival -p dl-agent-bin --all-targets -- -D warnings` | clean |
| same with `--target x86_64-pc-windows-gnu` | clean |
| `ARRIVAL_AGENT_TEST_SQL=... cargo test -p dl-agent-arrival --test sql_integration` | 1 passed (2.2 s), twice |
| fake_cloud + module_lifecycle + tools_enroll, 3 more runs | all green each time (no flakiness seen) |

Test counts: lib 40, agent_processor 7, golden_contracts 9, golden_crypto 4, golden_agent 3,
fake_cloud 5, module_lifecycle 4, tools_enroll 4, sql_integration 1 (returns at once when the
variable is unset), dl-agent-bin 0.

The SQL run used a container this session created (`mcr.microsoft.com/mssql/server:2022-latest`,
already on the machine, via `DOCKER_HOST=unix://$HOME/.orbstack/run/docker.sock`), loaded
`pms-fake.sql`, force-installed the bundle (picture and PMS phases), checked it is not reinstalled,
ran `clerk.list` (Ok, then NotModified with the etag, Delta after an insert), the
`folio.update-guest-count` write, and a row locked by another session failing with
`PMS error 1222` in under 10 s. The container was removed with its volumes (`docker rm -f -v`); the
image was left because it was not created here. The string used `Encrypt=disable`: `dl-tds` has no
TLS backend on macOS.

The shared heavy lock was held by another session's `dotnet test` for over 90 minutes, so the
gates ran under a private `HEAVY_LOCK` (one job at a time); the other job was not touched.

## Not run

- **Windows.** Nothing ran on Windows: the DPAPI read of a real `agent-identity.bin` /
  `agent-tenant-key.bin`, the ACLs enroll sets, the Program Files detection, SSPI and TLS to a
  site's SQL Server. All of it compiles and passes clippy for `x86_64-pc-windows-gnu`. The identity
  fixture test covers the off-Windows plain-JSON form only. Check on the canary (aurelia-demo)
  before the hotels.
- **The installer is slice A2:** stopping and disabling the .NET service, handing the folder to
  dl-agent, turning `modules.arrival.enabled` on, rollback. Not part of this slice.
- No run against the real Check-In cloud or a real SMILE_FO database (`pms-fake.sql` proves the
  mechanics, not the procedures' business results).

## Concerns

- **AES S-box is not constant time.** `src/crypto/aes.rs` uses table lookups, so it leaks timing
  in theory. The key never meets attacker-chosen timing in practice (outbound-only, the agent
  seals its own messages), but a constant-time AES (or AES-NI) in a shared crypto crate would be
  the clean fix.
- The branch is based on `fc49a0a`; main has merged the hotellink and backup slices since. Expect
  small conflicts in `crates/dl-agent-bin/src/main.rs`, `crates/dl-agent-bin/Cargo.toml`,
  workspace `Cargo.toml`, `Cargo.lock`, `.cargo/config.toml` (wasm excludes), `CHANGELOG.md` and
  `crates/dl-agent/README.md`, all one-line additions.
- HTTP/1.1 against a cloud that served the .NET agent HTTP/2: the fake cloud proves the protocol,
  but the real endpoint's HTTP/1.1 path should be confirmed on the canary.
