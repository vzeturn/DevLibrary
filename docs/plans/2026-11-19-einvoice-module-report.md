# E1 report: `einvoice` module in dl-agent (RT-0164 task 557, step 1)

Branch `feat/dl-agent-einvoice` (worktree `/Users/imac/Projects/DevLibrary-einvoice`), from
`main` 2d1de69. Not pushed.

Status: DONE_WITH_CONCERNS
Commit: `adbf7bf` feat(dl-agent-einvoice): Azolo E-Invoice as an opt-in dl-agent module (57 files)

## What changed

### New crate `crates/dl-agent-einvoice`

A port of `~/Projects/azolo-einvoice/rust/agent` (read only, not modified) as a dl-agent
module. It exports `einvoice_module() -> Factory` and `EinvoiceModule`.

- **Ported, unchanged in behaviour**:
  - `tcvn3`, `money`, `gojson`, `domain`, `pos`, `fo`, `legacy`, `dsn`, `install` (with
    `sql/collect/*.sql`), `outbox`, `config`, `agent_config` (`config.enc`, DPAPI
    LocalMachine), `sql` (+ `sql/drift.rs`).
  - `platform`: the agent API client.
  - `runner`: one collect / push / write-back cycle.
- **Adapted**:
  - `log.rs` writes through `dl-log` (target `einvoice`) and keeps the heartbeat ring
    buffer.
  - `runner.rs` no longer owns the loop. It exposes `run_cycle()` and returns a
    `CycleResult` with the failures classified (`sql_errors`, `item_errors`,
    `outbox_errors`, `push_failed`, `poll_failed`), so the module can map them to report
    codes and health.
  - `heartbeat_version()` = `dl-agent-<version>`.
  - `AgentConfig::load` returns `Ok(None)` when there is no `config.enc`.
  - Enroll and token exchange are not ported (E2).
- **New**:
  - `relay/`: a port of Go `internal/agentcollector/relay.go` + `internal/egressrelay`.
    - `wire.rs`: request/result JSON, 4 MiB request / 8 MiB response caps.
    - `allow.rs`: VNPT host suffix, https, port 443, no userinfo, POST/GET only, three
      forwarded headers, timeout 30 s default / 60 s cap, Go's Vietnamese refusal texts.
    - `transport.rs`: `Transport` trait; `HttpTransport` over dl-http with redirects off and
      the 8 MiB body cap.
    - `mod.rs`: the `Worker` poll loop:
      - Poll results: 204 → poll again; 404/405 → wait 5 min; device replaced → exit.
      - Poll errors back off 5 s doubling to 60 s, reset after a good poll.
      - Logs record id, host, status and duration only, never bodies.
    - Platform `relay_next` / `relay_result`.
  - `stats.rs`: atomic counters shared by the threads.
  - `module/settings.rs`: `config_dir` (default = Go `defaultConfigDir`) and
    `poll_interval_seconds`.
  - `module/collector.rs`: the collector thread, plus cycle → reports / degraded detail.
  - `module/mod.rs`: `EinvoiceModule`, which does the following:
    - It is opt-in.
    - Not enrolled → `set_degraded("not enrolled: no config.enc in <dir>")`. It re-checks
      every 30 s and answers commands while idle.
    - Enrolled → it spawns the collector and relay threads, calls `set_running` once both
      are up, and runs a 1 s module loop: commands, metrics, health, worker crash → `Err`
      (supervisor restart).
    - Stop → it signals the workers, waits up to 5 s, then detaches a worker still inside a
      blocking call. Process-wide `exclusive` locks keep an old worker and a new one from
      overlapping.
    - Device replaced → it stops the workers, stays degraded and reports critical once.
- Commands `einvoice.status` (JSON counters + `enrolled` + `detail`) and
  `einvoice.collect_now`.
- Metrics: `outbox_pending`, `pushed_total`, `last_push_at`, `relay_calls`,
  `relay_failures`, plus `cycles`, `read_total`, `errored_total`, `last_cycle_at`,
  `relay_refused`, `relay_poll_failures`.
- Reports:
  - Error: `einvoice_push_failed`, `einvoice_sql_failed`, `einvoice_outbox_failed`,
    `einvoice_config_unreadable`.
  - Warning: `einvoice_collect_failed`, `einvoice_writeback_failed`,
    `einvoice_invoices_rejected`, `einvoice_config_stale`, `einvoice_relay_refused`,
    `einvoice_relay_call_failed`, `einvoice_relay_poll_failed`,
    `einvoice_relay_result_failed`.
  - Critical: `einvoice_device_replaced`.

### dl-agent core (small, needed for "Disabled unless enabled = true")

- `Module::enabled_by_default()` (default `true`), plus
  `RemoteConfig::module_enabled_or(name, default)`. The supervisor reads the flag once per
  factory at `add`, and uses it when the section has no `enabled` key.
- Tests: `opt_in_modules_need_enabled_true` (config) and supervisor tests for opt-in
  modules.

### New package `crates/dl-agent-bin` (the `dl-agent` executable)

- `dl-agent-einvoice` depends on the `dl-agent` library, so the library package's own
  `[[bin]]` cannot register it without a dependency cycle.
- The binary (`src/main.rs`, `git mv`) moved to `dl-agent-bin`, which registers
  `.module(dl_agent_einvoice::einvoice_module())`. The binary name is still `dl-agent`.
- `scripts/build-dl-agent.sh` now builds `-p dl-agent-bin --bin dl-agent`. The dl-agent
  README release command was updated to match.

### Docs

- `crates/dl-agent-einvoice/README.md`: scope, config keys, behaviour, codes, differences
  from Go, tests.
- `crates/dl-agent/README.md`: new "Modules" section.
- `crates/dl-agent/PROTOCOL.md` §8: opt-in note.
- `CHANGELOG.md` Unreleased → Added: three entries.

## Deviations from the brief / Go, and why

1. **The binary moved to `dl-agent-bin`** (the dependency cycle, see above). Anything that
   built `-p dl-agent --bin dl-agent` must use `-p dl-agent-bin`. Only the build script and
   the README did.
2. **The opt-in needed a core change** (`enabled_by_default`). Without it every module
   without a section runs.
3. **State location.** `ModuleContext::data_dir()` is already `<data>/modules/einvoice`, so
   nothing is nested further. The outbox is `outbox.json` there. The sealed config's
   `outbox_path` is ignored.
   - A Go `outbox.db` is never read: its pending items are re-collected from SQL, and
     write-back is idempotent, as in the Rust port.
4. **SQL**: the module uses its own dl-tds connections built from the `config.enc` DSNs, not
   `ctx.sql()` pools. The DSNs live in `config.enc`, and the legacy 2008 handling is part of
   the port. As a result the dl-agent breaker / transport ladder does not apply to this
   module.
5. **Heartbeat** `agent_version` = `dl-agent-<version>` (Go: `""`, Rust port:
   `rust-0.1.0`). No self-update; platform update hints are ignored.
6. **Not ported (E2)**: enroll, bootstrap, `-sql-prompt`, token exchange,
   `transport_live.rs` (it exercised the enroll TLS ladder). No default SQL password anywhere.
7. **Relay read errors**: a failure reading the provider's answer body maps to the transport
   error text (Go reports `io.ReadAll` errors the same way). An answer over 8 MiB becomes
   "trả lời của cổng nhà cung cấp quá lớn để chuyển tiếp".
8. **Struct field docs**: the workspace lints `missing_docs`.
   - Small structs got real field docs.
   - The large structs that mirror Go structs field for field (`AgentConfig`, `InvoiceHeader`,
     `PushItem`, the FO/POS raw rows, …) carry `#[allow(missing_docs)]` with that reason,
     as `dl-time` does for `Month`.

## TCVN3 / money fidelity and `dl-vn`

- TCVN3 and money are the Go ports, unchanged. `tests/golden.rs` (800 cases) and
  `tests/legacy_golden.rs` pass byte for byte.
- Experiment, which golden cases would differ with dl-vn:
  - Method: a throwaway test, with `dl-vn` as a dev-dependency, ran over every string in the
    inputs of `golden.json` (`pos`, `fo_header`, `fo_lines`, `fo_detail`, `checks`). It
    compared `tcvn3::convert(s)` with `dl_vn::tcvn3::decode(s, CodePage::Cp1252)`, followed
    by the same NBSP→space and trailing-space trim that `convert` does.
  - Result: **0 of 800 cases differ; 0 of 69,206 strings.**
  - Coverage: 7,686 of those strings, across 798 cases, are non-ASCII. They contain 20
    distinct characters: the TCVN3 bytes `¨ « ¶ ¸ Õ Ý ß á è é ñ ø ù`, already-Unicode `ặ ẻ
    ủ` and CJK `日本`.
  - Not covered: the TCVN3 letters that do not appear in the vectors. A move to dl-vn
    should add vectors for the full table first.
  - The test and the dev-dependency were reverted before the commit, and Cargo.lock has no
    `dl-vn` edge.
  - The crate README states this.

## Tests and gates

All runs were in `/Users/imac/Projects/DevLibrary-einvoice`. The final run was one
`heavy.sh` slot, after the dl-vn revert.

| Command | Result |
|---|---|
| `cargo fmt --all` then `cargo fmt --all --check` | exit 0 |
| `cargo clippy -p dl-agent-einvoice -p dl-agent -p dl-agent-bin --all-targets -- -D warnings` | exit 0 (an earlier run flagged `type_complexity` in `tests/module_lifecycle.rs`; fixed with a `Seen` alias) |
| same `--target x86_64-pc-windows-gnu` | exit 0 |
| `cargo test -p dl-agent-einvoice -p dl-agent -p dl-agent-bin` | exit 0 |

Test results:
- dl-agent unit: 122 passed, 2 ignored.
- dl-agent `sql_live`: 4 passed. The other dl-agent integration test: 2 passed, 1 ignored.
- dl-agent-bin: 0 tests.
- dl-agent-einvoice:
  - unit: 65 passed.
  - `golden`: 1 test, 800 cases.
  - `legacy_golden`: 3 passed.
  - `legacy_live`: 1 passed (skips without `MMEI_TEST_MSSQL_URL`).
  - `module_lifecycle`: 4 passed.

Checked as well:
- Every `.rs` file is ≤ 800 lines. The largest is `crates/dl-agent/src/agent.rs` at 755,
  unchanged. In the new crate the largest is `runner.rs` at 711.
- `rg` finds no default or embedded SQL password in `src`, `tests` or `sql`. The only
  passwords are DSN escaping vectors in `dsn.rs` tests.

Not run:
- `cargo check -p dl-agent-einvoice -p dl-agent-bin --target wasm32-unknown-unknown` fails
  inside the **dl-agent library** at `service_watch.rs`. That file uses `crate::cli` and
  `hub::HttpTransport`, which are cfg'd out on wasm32-unknown. The failure was already there
  at base 2d1de69: this commit touches neither file.
  - So `cargo wasm`, which builds the whole workspace, fails with or without this branch.
  - The new crate's own wasm build cannot be judged until that is fixed. dl-agent-einvoice
    and dl-agent-bin may need `--exclude` in the alias.

## Not done / gaps

- Not run against a real SMILE database or the real platform. The enrolled lifecycle test
  uses a fake platform on a local socket and an unreachable SQL Server (port 1).
- No live relay call to a VNPT portal. Relay tests use fakes and the real platform client
  against a fake hub.
- Windows was only cross-clippy'd (`x86_64-pc-windows-gnu`): not run on Windows, not
  run as a service.
- Live SQL tests (`MMEI_TEST_MSSQL_URL`) were not run (no server here). They skip without
  the variable.
- The workspace-wide gate (`cargo check-all`, `cargo wasm`, full test suite) was not run.
  That is left to the main session per the brief.

## Concerns

- **Merge with current `main` conflicts in docs only.** `main` moved from 2d1de69 to
  8857b8f (PR #19 setup sub-commands, PR #20 plans).
  - `git merge-tree main HEAD` reports conflicts in `CHANGELOG.md` and
    `crates/dl-agent/README.md`. Both are additive and need both sides kept.
  - `main` now also tracks `docs/plans/2026-11-19-einvoice-module-brief.md`, which is
    untracked in this worktree.
  - Not rebased, so the reviewed diff stays against its stated base.
- **The executable moved to `dl-agent-bin`.** Release or install tooling outside this repo
  that runs `cargo build -p dl-agent --bin dl-agent` must switch. Inside the repo, only
  `scripts/build-dl-agent.sh` and the README did.

- A worker stuck inside a blocking SQL / HTTP call outlives a stop by up to its own timeout
  (SQL query timeout, platform 30 s, provider ≤ 60 s). The module returns within about 5 s.
  A restart waits on the process-wide lock before touching the outbox.
- Switching a site from the Go service to the module: stop the Go service first. Both
  running would double-push (the platform dedups by fingerprint) and fight over the relay.
