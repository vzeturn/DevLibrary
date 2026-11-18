# Brief P1: `pms-site` module in dl-agent (RT-0164 task 557, step 2)

## Goal

The SMILE PMS site agent (`pms-agent`, service `SmilePmsAgent`) moves onto the shared `dl-agent`,
following the pattern of the `einvoice` module (`crates/dl-agent-einvoice`, read its README and
`src/module/`). This slice builds the **module only**, inside DevLibrary. It does not change
SMILE-PMS, release anything, or deploy.

## Source to port

- `~/Projects/SMILE-PMS/apps/site-agent/rust/`: the Rust port of the Go agent (`apps/site-agent`),
  about 13k lines, DevLibrary crates only (pinned to old rev `3b50836`). Read its README first.
  It covers enroll + keystore (`agent.key` DPAPI machine scope / 0600 file, `identity.json`),
  the signed token exchange and JWT refresh, the WebSocket stream (`/agent/v1/stream`: hello,
  ping/pong, ops, ≤ 8 in flight, close codes, back-off), the SQL executor (allow-listed procs from
  the embedded ops manifest, binds, request-key log `MM_Op_GetLog`/`MM_Op_CreateLog`, deadlock
  retry, result caps), pools over `databases.json` (MM_PMS, SMILE_FO, SMILE_BO, SMILE_POS),
  signed migration bundles (`bundle`, `migrate`, `remotemigrate`: backup gate, drift check, dry
  run), `retention` (MM_OpLog purge), `acl`, `log`, `svc`, `update`.
- The Go agent (`apps/site-agent`) is the reference whenever the two disagree.
- The ops manifest is embedded from `apps/site-agent/internal/ops/ops-manifest.json`. Copy it
  into the new crate (`assets/`), with a test that fails when it drifts from a path given by env
  `PMS_OPS_MANIFEST` (skip when unset), so a SMILE-PMS change is noticed.

## Target

- New crate `crates/dl-agent-pms`, library exporting `pms_site_module() -> Factory`. Same lint
  and edition settings as the other crates; workspace crates by path, not the old git rev.
  Register it in `crates/dl-agent-bin/src/main.rs` (`App::module`). It stays `Disabled` unless
  the remote config has `modules.pms-site.enabled = true`.
- **Drop what dl-agent owns:** `svc`, `update` (self-update), the CLI `install/uninstall`, and the
  service log rotation. Ignore update hints from the PMS cloud. Keep `acl` for the PMS state dir.
- **Keep the PMS identity and state where they are**, so a site running `SmilePmsAgent` switches
  with no re-enroll: config file, state dir, `agent.key`, `identity.json`, `databases.json` read
  and written exactly as the port does (`config.rs` defaults: `%ProgramData%\SmilePMS\Agent`).
- Module config `modules.pms-site`: `enabled`; `config_path` (optional, default as the port);
  `state_dir` (optional). Module-local scratch under `ctx.data_dir()/pms-site/`.
- `Module::run`:
  - No identity and no enrollment code → `set_degraded("not enrolled: …")` and wait on
    `ctx.stop()` (not an error loop). With an enrollment code in the config → enroll as today.
  - Otherwise run the stream loop and the retention schedule; `set_running` once the stream is
    up (degraded while reconnecting longer than one back-off cycle, with the reason).
  - Stop promptly on `ctx.stop()`: close the stream with the normal close code, let in-flight ops
    finish up to a short grace (≤ 5 s), then cancel.
  - Metrics: ops in flight / ok / rejected / failed, last op time, reconnects, migrations applied.
  - Reports with stable codes: `pms_stream_failed`, `pms_auth_failed`, `pms_sql_failed`,
    `pms_migration_failed`, `pms_op_rejected` (rate-limited, no SQL text or bind values).
  - The `hello` frame's agent version is `dl-agent-<version>`.
- Sub-command `dl-agent pms-migrate -bundle <dir> [-dry-run] [-check-drift] [-rerun-partial n]
  [-config path]` via `App::tool` (port of `pms-agent migrate`). Optional `pms-enroll --cloud-url
  … --code …` that writes the config file the way the port expects (needed by the installer
  slice); skip it when the config already has an identity, `--force` to redo.
- SQL: keep the port's pools for this slice (proven by `sql_integration`). Note in the report
  what moving to dl-agent's shared `sql` pools (`ctx.sql(name)`) would take. `databases.json` →
  DPAPI is out of scope; note it as a gap.
- Remote commands, if cheap: `pms.status` (counters JSON), `pms.reconnect`.

## Tests (mirror source structure, hand-written fakes, no mocking crates)

- Port the unit tests and the fixture tests: `wire_fixtures` (27 golden frames),
  `ops_contract`, `bundle_golden`. Keep `sql_integration` env-gated (`PMS_AGENT_TEST_SQL`) and
  run it once against a local SQL Server 2022 container (`mcr.microsoft.com/mssql/server:2022-latest`,
  remove it afterwards).
- A fake PMS cloud on a local socket (the port's `fakecloud` / e2e scenario): enroll, token,
  hello, op round trip, reconnect after a close.
- Module lifecycle: disabled; not enrolled → degraded; stop is prompt with an op in flight.

## Checks before commit

- `~/.cargo/bin/cargo fmt --all`
- `~/.cargo/bin/cargo test -p dl-agent-pms -p dl-agent-bin`
- `~/.cargo/bin/cargo clippy -p dl-agent-pms -p dl-agent-bin --all-targets -- -D warnings`, and the
  same with `--target x86_64-pc-windows-gnu`.
- Every file ≤ 800 lines (200–400 typical). No swallowed errors.

## Rules

- Work only in the given worktree, branch `feat/dl-agent-pms`. Conventional Commits in English
  with the trailer below. **Do not push, merge or deploy.** Do not touch SMILE-PMS.
- Heavy builds through `~/.claude/scripts/heavy.sh <cmd>`. Don't dump logs. Remove any Docker
  container you create.
- Update `crates/dl-agent/README.md` (Modules list), `CHANGELOG.md` (Unreleased → Added) and the
  crate README (scope, config, differences from the Go agent, switch-over steps for a live site).
- Report (≤ 10 lines) to `docs/plans/2026-11-19-pms-site-module-report.md`: status, SHA, one test
  line, gaps.

Commit trailer:

```
Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k
```
