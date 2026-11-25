# Brief H1: `hotellink` module in dl-agent (RT-0164 task 557, step 3)

## Goal

The SMILE Hotel Link site agent (Go, `~/Projects/sr-hotel-link-api/go`, Windows service `SmileHLAgent`,
binary `smile-hl-agent.exe`) moves onto the shared `dl-agent` as module `hotellink`, following the
pattern of `crates/dl-agent-einvoice` and `crates/dl-agent-pms` (read their READMEs and `module/`).
This slice builds the **module only**, inside DevLibrary. Do not change SMILE-HLAPI, release, or deploy.

## Source to port (Go is the reference; read `~/Projects/sr-hotel-link-api/CLAUDE.md` first)

- `go/internal/agent/`: `config.go` (LocalConfig, DPAPI `enc:` values, UTF-8 BOM strip, seed-once
  encrypt of plaintext), `hubclient.go` (Bearer token + `X-Hotel-Id`, `X-Agent-Version`,
  `X-Instance-Id`, `X-Mapping-Hash`; hub URL fallbacks with sticky active base), `run.go`
  (booking long-poll loop, processJob, ack + re-ack of unacked, drain pending, mapping sync +
  mapping commands, availability loop, heartbeat, hub config/policy with local cache),
  `reconcile.go` (the Folio/FolioRate/AdditionalName diff/merge, cancel policy, manual conflicts,
  guards — the riskiest part, port faithfully), `jobqueue/` (durable local queue), `breaker/`
  (PMS circuit breaker), `liveness.go` + `supervise.go` (loop stall detection, panic guard),
  `logfile.go` (drop: dl-agent owns logs).
- `go/internal/pms/` (repo over SMILE_FO: entities, insert, mapping, truncation, availability),
  `go/internal/jobmodel/` (wire contract with the hub: keep JSON field names byte-compatible).
- **Drop what dl-agent owns:** service host, `update/` (self-update; ignore update hints from the
  HL hub, never call CheckUpdate/DownloadBlob/AckUpdate), rolling log file, `cmd/agent` CLI.
- **Defer `configui/`** (local web UI on 127.0.0.1:8088) to a later slice; hub mapping commands
  still work. Note it as a gap in the report.

## Target

- New crate `crates/dl-agent-hotellink`, library exporting `hotellink_module() -> Factory`; same
  lints/edition as the other crates; workspace crates by path. Register in
  `crates/dl-agent-bin/src/main.rs`. `Disabled` unless remote config `modules.hotellink.enabled`.
- **Keep the HL identity and config where they are** so a site running `SmileHLAgent` switches
  with no re-provisioning: read (and seed-once encrypt) `%ProgramData%\SMILE-HL-Agent\config.json`
  exactly as Go does — DPAPI machine scope (`CRYPTPROTECT_LOCAL_MACHINE`, no entropy), `enc:`
  prefix, same base64. Reuse the DPAPI helper in `dl-winsvc` (`dpapi.rs`); off Windows use the
  test seam the other crates use. Module config `modules.hotellink`: `enabled`, `config_path`
  (optional). Local queue/state under `ctx.data_dir()/hotellink/` — but if Go's queue dir exists
  with pending or unacked jobs, import them once (or read in place) so no booking is lost on switch.
- SQL: build the connection from the HL config (`sql` section: server/database/trusted/user/password
  or `connectionStringOverride`) on dl-agent's TDS stack (`dl-tds`, see how `dl-agent-pms` and
  `ctx.sql` build pools). SQL Server 2005–2022 must keep working (the einvoice/pms crates handle
  old TLS/unencrypted login — reuse, don't re-invent). Parameterized SQL only.
- `X-Agent-Version` = `dl-agent-<version>`; keep `X-Instance-Id` single-active semantics.
- `Module::run`: config missing/no token → `set_degraded("not configured: …")` and wait on
  `ctx.stop()`. Otherwise run the loops; `set_running` once the first poll succeeded; degraded with
  reason when the PMS breaker is open, the hub is unreachable past one back-off, or a loop stalls.
  Stop promptly: finish the job in flight (≤ 5 s grace, it is one SQL transaction), then cancel;
  unfinished jobs stay in the durable queue for re-ack.
- Metrics: jobs polled/applied/acked/failed, manual conflicts, queue depth, unacked, availability
  pushes, mapping hash, last job time, breaker state. Reports with stable codes:
  `hl_hub_failed`, `hl_auth_failed`, `hl_pms_failed`, `hl_reconcile_failed`, `hl_manual_conflict`
  (rate-limited; no guest names, no SQL text, no token).
- Sub-command `dl-agent hl-enroll --hub-url … --hotel-id … --instance-id …` (token read from env
  `DL_AGENT_HL_TOKEN` or stdin, never argv; optional `--sql-user`, password the same way) that
  writes config.json the way the HL install script does (server `.`, database `SMILE_FO`), DPAPI
  encrypted; skip when a config with a token exists unless `--force`. Needed by the installer slice.
- Remote commands if cheap: `hl.status` (counters JSON), `hl.sync-mapping`.

## Tests (mirror source structure, hand-written fakes, no mocking crates)

- Port the Go unit tests of `agent` (reconcile above all), `pms`, `jobmodel`, `jobqueue`, `breaker`.
  Keep `jobmodel` JSON golden-compatible with Go (fixtures from Go tests).
- A fake HL hub on a local socket: poll → job → apply against a fake repo → ack; re-ack after a
  restart; mapping push; availability push; 401 → degraded.
- SQL integration, env-gated (`HL_AGENT_TEST_SQL`), run once against a local SQL Server 2022
  container with `go/deploy/smile-schema.sql` loaded (remove the container afterwards; use
  `DOCKER_HOST=unix://$HOME/.orbstack/run/docker.sock`; do not touch containers you did not create).
- Module lifecycle: disabled; not configured → degraded; prompt stop with a job in flight.

## Checks before commit

- `~/.cargo/bin/cargo fmt --all`
- `~/.cargo/bin/cargo test -p dl-agent-hotellink -p dl-agent-bin`
- `~/.cargo/bin/cargo clippy -p dl-agent-hotellink -p dl-agent-bin --all-targets -- -D warnings`,
  and the same with `--target x86_64-pc-windows-gnu`.
- Every file ≤ 800 lines (200–400 typical). No swallowed errors. Heavy builds via
  `~/.claude/scripts/heavy.sh`.

## Rules

- Work only in worktree `~/Projects/DevLibrary-hl`, branch `feat/dl-agent-hotellink`. Conventional
  Commits in English, trailer:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k`.
  **Do not push, merge or deploy.** Do not modify SMILE-HLAPI. Never print secrets.
- Report to `docs/plans/2026-11-19-hotellink-module-report.md`: what was ported, what differs from
  Go and why, gaps (configui, Windows not run), test counts.
