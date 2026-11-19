# Brief B2a: `backup` module in dl-agent (RT-0164 task 557, step 4)

## Goal

The SRBackup site agent (Go, `~/Projects/SRBackup/agent`, ~7.8k non-test lines) moves onto the
shared `dl-agent` as module `backup`, following the pattern of `crates/dl-agent-einvoice` and
`crates/dl-agent-pms` (read their READMEs and `module/`). This slice builds the **module only**,
inside DevLibrary. Do not change SRBackup, release, or deploy.

Production has **no SRBackup agents yet** (0 agents, hub `https://backupapi.sr.io.vn`), so there
is no `agent.json` / bbolt queue to import: the module starts clean. Do not port the Go on-disk
formats or the hwid-derived `secret` package.

## Hub contract (keep byte-compatible; the hub is `~/Projects/SRBackup/hub`, Hono on Workers)

Read `agent/internal/hubclient/` and `hub/src/routes/agent.ts` + `agentDistribution.ts`:
`POST /api/agent/register` / `self-register` (enroll key → agent id + token), `GET /api/agent/config`
(servers, databases, schedules with embedded cloud destination, restore requests, commands),
`GET /api/agent/master-key`, `POST /api/agent/report`, `POST /api/agent/discover`,
`POST /api/agent/logs`. Since 2026-11-19 `/master-key` returns a **per-agent derived key** and
`/config` re-seals every credential under it; a value the hub cannot open arrives as `""` with
`credentialError: true` (see `hub/src/lib/agentKey.ts`). Credential format is
`base64url(salt).iv.ciphertext`, AES-256-GCM, key = HKDF-SHA256(ikm = key string, salt, empty info)
(`agent/internal/hubclient/decrypt.go`). On a decrypt failure re-fetch the key once and retry
(`syncloop/masterkey.go`). `credentialError` → that job/command fails with a clear reason and a
report, the rest keeps running.

## Port (Go is the reference)

- `hubclient`, `syncloop` (config poll, master key, commands such as TestConnection, restore
  requests, outbox of reports so a hub outage loses nothing), `scheduler` (cron), `executor`
  (concurrency limit, lease, dead letter), `queue` (durable, under `ctx.data_dir()/backup/`;
  any SQL password in it sealed with the same at-rest protection dl-agent uses for einvoice
  `config.enc` — DPAPI machine scope on Windows), `backup` (pipeline + restore), `retention`,
  `discovery`, `jobhistory`.
- Uploaders in this slice: **local folder, S3-compatible (incl. Cloudflare R2, multipart), Azure
  Blob** behind one uploader trait. **Google Drive and OneDrive (OAuth refresh) are B2b**: the
  trait must fit them; a schedule targeting them fails with `backup_destination_unsupported` until
  then. Use DevLibrary's own crates (`dl-http`, `dl-tls`, `dl-hash`, …) per the repo's dependency
  policy; read the root `CLAUDE.md`.
- **Drop what dl-agent owns:** service host, `updater`, `logger`, `health`, `metrics` (map to
  dl-agent metrics), `hwid`, `secret`, `cmd/agent`.

## SQL

- Servers come from the hub config (host, port, instance, auth, sealed password), not `sql.json`.
  Build connections on `dl-tds` the way `dl-agent-pms`/`ctx.sql` do (old TLS, unencrypted login
  for SQL 2005–2012 — reuse, don't re-invent). `authType: Windows` → `dl-tds` SSPI.
- Statements are built from validated, bracket-quoted identifiers and N-quoted paths (port Go's
  quoting and path checks exactly); everything that can be a parameter is one.
- **Fix a real bug while porting:** Go always sends `WITH COMPRESSION`, which fails on SQL Express
  / Web and on SQL 2005 / 2008 non-Enterprise. Read edition + version once per connection
  (`SERVERPROPERTY('EngineEdition')`, `ProductVersion`, or the dl-agent site survey) and only add
  `COMPRESSION` where supported; record the choice in the job report. Same care for `CHECKSUM`
  and differential/log backups on a database in SIMPLE recovery (log backup → clear
  `backup_recovery_model` failure, not a raw SQL error).
- The `.bak` file is written by the SQL Server service; when the server is remote the path is on
  that machine. Keep Go's semantics and its "file not reachable from the agent" error.

## Module, CLI, reports

- New crate `crates/dl-agent-backup`, `backup_module() -> Factory`, registered in
  `crates/dl-agent-bin/src/main.rs` (the hotellink slice also edits that file in another branch;
  keep your change to one line + one `use`). `Disabled` unless `modules.backup.enabled`.
- Sub-command `dl-agent backup-enroll --hub-url <https origin>` with the enroll key from env
  `DL_AGENT_BACKUP_KEY` or stdin (never argv); stores agent id + token sealed in the data dir;
  skip when already enrolled unless `--force`. Needed by the installer slice (B3).
- `Module::run`: not enrolled → `set_degraded("not enrolled")`, wait on `ctx.stop()`.
  `set_running` after the first config poll; degraded with reason when the hub is unreachable
  past one back-off or auth fails. Stop: let an in-flight `BACKUP` finish up to a short grace,
  then cancel via TDS attention; the job stays queued.
- Metrics: queue depth, jobs ok/failed/dead-lettered, bytes uploaded, last success per database.
  Reports with stable codes, rate-limited, no passwords/tokens/connection strings:
  `backup_hub_failed`, `backup_auth_failed`, `backup_sql_failed`, `backup_upload_failed`,
  `backup_credential_error`, `backup_destination_unsupported`, `backup_recovery_model`.
- Remote commands if cheap: `backup.status`, `backup.run-now <scheduleId>`.

## Tests (mirror source structure, hand-written fakes, no mocking crates)

- Port the Go unit tests that cover the ported packages (executor, queue, scheduler, pipeline,
  restore, retention, syncloop, masterkey, uploader request signing). Credential decrypt must
  pass against values produced by the hub's `encryptCredential` (fixtures: generate a few with
  `cd ~/Projects/SRBackup/hub && bun` + `src/lib/credentialCrypto.ts`, test-only keys).
- Fake hub on a local socket: enroll → config → scheduled job → report; key rotation (first
  decrypt fails → re-fetch → ok); `credentialError` → job fails, others run; hub down → outbox.
- S3 SigV4 against the AWS published test vectors; Azure SharedKey/SAS signing against fixed
  vectors.
- SQL integration, env-gated (`BACKUP_AGENT_TEST_SQL`), run once against a local SQL Server 2022
  container: full backup with and without compression, log backup on SIMPLE → `backup_recovery_model`,
  restore WITH REPLACE to a new name. Remove the container afterwards; use
  `DOCKER_HOST=unix://$HOME/.orbstack/run/docker.sock`; never touch containers you did not create.
- Module lifecycle: disabled; not enrolled → degraded; prompt stop with a job in flight.

## Checks before commit

- `~/.cargo/bin/cargo fmt --all`
- `~/.cargo/bin/cargo test -p dl-agent-backup -p dl-agent-bin`
- `~/.cargo/bin/cargo clippy -p dl-agent-backup -p dl-agent-bin --all-targets -- -D warnings`,
  and the same with `--target x86_64-pc-windows-gnu`.
- Every file ≤ 800 lines (200–400 typical). No swallowed errors. Heavy builds via
  `~/.claude/scripts/heavy.sh`.

## Rules

- Work only in worktree `~/Projects/DevLibrary-bk`, branch `feat/dl-agent-backup`. Conventional
  Commits in English, trailer:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k`.
  **Do not push, merge or deploy.** Do not modify SRBackup (its main checkout has another
  session's uncommitted work — read only). Never print secrets.
- Report to `docs/plans/2026-11-19-backup-module-report.md`: what was ported, what differs from Go
  and why (compression fix, no on-disk import), gaps (Drive/OneDrive = B2b, Windows not run),
  test counts.
