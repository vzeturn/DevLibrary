# Brief A1: `arrival` module in dl-agent (RT-0164 task 557, step 5)

## Goal

The Azolo Check-In site agent (`~/Projects/SmileCheckin/src/MM.Arrival.Agent`, .NET 10, Windows
service `MM.Arrival.Agent`, wire contract in `src/MM.Arrival.Sync.Contracts`) moves onto the shared
`dl-agent` as module `arrival`, following the pattern of `crates/dl-agent-einvoice` and
`crates/dl-agent-pms` (read their READMEs and `module/`). This slice builds the **module only**,
inside DevLibrary. Do not change SmileCheckin, release, or deploy.

**Three hotels run this agent in production** (muine-bay-resort, vic-saigon, victory-vungtau) and a
demo tenant (aurelia-demo) will be the canary. A switch must need **no re-enrollment**: read the
existing install in place. Read `~/Projects/SmileCheckin/CLAUDE.md` and `docs/saas/` first.

## Keep in place (find the exact paths in `Program.cs`, `AgentOptions.cs`, `deploy/agent/Install.ps1`)

- `appsettings.Production.json` in the install directory: `CloudBaseUrl`, `PmsConnectionString`
  (may be DPAPI-protected — follow how the .NET agent reads it), watches, timeouts, concurrency.
- `agent-identity.bin`: DPAPI **LocalMachine + entropy** `MM.Arrival.Agent/identity/v1`
  (`AgentIdentity.cs`); plain 0600 JSON off Windows. `agent-tenant-key.bin` (`TenantKeyVault.cs`),
  `vault/` (LegalVault), `agent-idempotency.jsonl` (48 h journal). Read them where they are, or
  move them once into `ctx.data_dir()/arrival/` with the same protection — your choice, explain it.
  Reuse the DPAPI helper in `dl-winsvc` (`dpapi.rs`, add entropy support if missing) and the test
  seam the other crates use off Windows.
- Module config `modules.arrival`: `enabled`, `install_dir` (optional override).

## Port

- `CloudLinkWorker` (HTTP/2 long poll), `RequestProcessor`, `PmsExecutor` (allow-listed procs only,
  timeouts, lock timeout, concurrency), `PmsProcedureInstaller` (bundle install — reuse dl-agent's
  signed proc-bundle machinery from C5 where it fits; keep the .NET bundle semantics otherwise),
  `PmsConnectionProbe`, `ChangeWatcher` (snapshots/ETag, capacity), `IdempotencyLog`, `LinkStatus`.
- From Contracts: `SyncEnvelope` (AES-GCM envelopes, HKDF per direction, ±120 s window),
  `SyncMessages`, `CanonicalHasher`, `ResultSetDelta`, `PmsProjection`, `PmsRequestValidator`,
  `PmsErrors`, `TenantKeyVault` (wrap/unwrap), `LegalVaultOps`, and the **`PmsOperationCatalog`**
  (the agent's security boundary: it refuses any op not in it). Port the catalog as data:
  write a tiny throwaway exporter (a test or `dotnet run` script in a scratch dir, not committed to
  SmileCheckin) that dumps the .NET catalog to JSON, commit that JSON in the crate, and add a test
  that loads it. Note in the report how a catalog change reaches sites (dl-agent release).
- **Byte compatibility is the bar:** envelopes, canonical hashes, deltas and projections must be
  identical to .NET. Produce golden fixtures from the .NET code (same throwaway exporter) and test
  against them, the way the einvoice port used 800 golden cases.
- SQL: the PMS connection string → `dl-tds` (SQL 2005–2022, old TLS, SSPI for integrated security;
  see `dl-agent-pms` and `ctx.sql`). Parameterized only. TCVN3 via `dl-vn` where the .NET code
  converts text.
- **Drop what dl-agent owns:** `Update/`, `SelfCare/`, `Diagnostics/` (ProblemJournal, EventLog
  history → dl-agent reports), service host.

## Module, CLI, reports

- New crate `crates/dl-agent-arrival`, `arrival_module() -> Factory`, registered in
  `crates/dl-agent-bin/src/main.rs` (hotellink and backup slices edit that file in other branches;
  keep your change to one line + one `use`). `Disabled` unless `modules.arrival.enabled`.
- Sub-command `dl-agent arrival-enroll --cloud-url <https origin>` taking the enrollment token from
  env `DL_AGENT_ARRIVAL_TOKEN` or stdin (never argv) and the PMS connection string the same way
  (`DL_AGENT_ARRIVAL_PMS`), writing identity + settings exactly as the .NET installer would; skip
  when an identity exists unless `--force`. Needed by the installer slice (A2).
- `Module::run`: no identity → `set_degraded("not enrolled")`, wait on `ctx.stop()`. `set_running`
  after the first successful poll; degraded with reason when the cloud is unreachable past one
  back-off, auth/envelope fails (clock skew → say so), or the PMS is down. Stop: finish in-flight
  ops up to a short grace, idempotency journal flushed.
- Metrics: requests ok/failed/refused, in flight, snapshot count, last poll, PMS latency.
  Reports with stable codes, rate-limited, no guest data, no SQL text, no keys:
  `arrival_cloud_failed`, `arrival_auth_failed`, `arrival_clock_skew`, `arrival_pms_failed`,
  `arrival_op_refused`, `arrival_proc_install_failed`.

## Tests (mirror source structure, hand-written fakes, no mocking crates)

- Port the agent and contracts unit tests from `~/Projects/SmileCheckin/tests` that cover what you
  ported. Golden fixtures from .NET as above.
- Fake cloud on a local socket: poll → request → PMS fake → envelope reply; replayed request →
  idempotent; skewed clock → `arrival_clock_skew`; 401 → degraded.
- Identity read: a fixture `agent-identity.bin` written as the .NET code writes it (off-Windows plain
  JSON form; DPAPI path behind the existing test seam).
- SQL integration, env-gated (`ARRIVAL_AGENT_TEST_SQL`), once against a local SQL Server 2022
  container with the repo's `srCI_*` procs if a script exists; remove the container afterwards; use
  `DOCKER_HOST=unix://$HOME/.orbstack/run/docker.sock`; never touch containers you did not create.
- Module lifecycle: disabled; not enrolled → degraded; prompt stop.

## Checks before commit

- `~/.cargo/bin/cargo fmt --all`
- `~/.cargo/bin/cargo test -p dl-agent-arrival -p dl-agent-bin`
- `~/.cargo/bin/cargo clippy -p dl-agent-arrival -p dl-agent-bin --all-targets -- -D warnings`,
  and the same with `--target x86_64-pc-windows-gnu`.
- Every file ≤ 800 lines (200–400 typical). No swallowed errors. Heavy builds via
  `~/.claude/scripts/heavy.sh`.

## Rules

- Work only in worktree `~/Projects/DevLibrary-ar`, branch `feat/dl-agent-arrival`. Conventional
  Commits in English, trailer:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k`.
  **Do not push, merge or deploy.** Do not modify SmileCheckin (read only; exporters live in your
  scratch dir). Never print secrets, connection strings or guest data.
- Report to `docs/plans/2026-11-19-arrival-module-report.md`: what was ported, file-placement
  decision, what differs from .NET and why, gaps (Windows not run, installer = A2), test counts.
