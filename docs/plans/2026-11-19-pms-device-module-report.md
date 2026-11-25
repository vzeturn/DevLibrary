# pms-device module: report

Brief: `docs/plans/2026-11-19-pms-device-module-brief.md` (RT-0164, task 557 step 2).
Branch `feat/dl-agent-pms-device` from main `9fe172e`. Not pushed.

## What changed

New crate `crates/dl-agent-pms-device` (package `dl-agent-pms-device`, module `pms-device`):

- `src/adapters`, `src/agent`, `src/config`, `src/contracts`, `src/native`, `src/queue`: the
  SMILE-PMS Rust port (`apps/device-agent/rust/src`) copied without its `host/` (Windows
  service, console host, Event Log filter, service log file) and `main.rs`. Changes to the
  copied code: observer hooks in `agent/worker.rs` (each poll) and `agent/processor.rs`
  (claimed / finished), `ResultOutbox::pending_count`, `pub const MAX_BACKOFF_SECS`.
- `src/module/`: the dl-agent side.
  - `mod.rs`: `PmsDeviceModule` (opt-in), `pms_device_module() -> Factory`,
    `pms_device_module_with(Hooks)` for tests. A cycle prepares the settings, starts the worker
    thread, maps API health to `running` / `degraded`, answers commands, restarts the worker
    when an appsettings file or the token file changes (30 s check), and on stop gives the worker
    5 s and leaves a blocked device call behind.
  - `settings.rs`: module keys `enabled`, `appsettings_dir` (default
    `%ProgramW6432%\SmilePms\DeviceAgent`, off Windows the module data dir), `overrides`
    (merged over the `Agent` section).
  - `prepare.rs`: loads `appsettings.json` + `appsettings.{env}.json` + `Agent__*` + overrides
    with the port's .NET configuration rules, binds and validates, forces `RunOnce` off, reads
    the token once (the error names the path only).
  - `telemetry.rs`: counters by kind (`claimed` / `ok` / `failed`, unknown kinds as `other`),
    `last_command_unix`, API health (down after 30 s = the worker's longest back-off, at once
    on 401 / 403), reports with a 10 min per-code limit and a `held_back` count.
  - `commands.rs`: `pms-device.status`, `pms-device.flush_outbox` (on its own thread, one at a
    time); metrics publishing incl. `outbox_pending`.
  - `logger.rs`: the port's `Logger` writing to `dl-log` target `pms-device`, same messages,
    `category` and `exception` fields.
- `tests/`: the port's `tests/*.rs` except `host_run.rs` (tests the dropped host);
  `pos_fakes.rs` became `pos_fakes/mod.rs` (included with `#[path]`, so `missing_docs` does not
  hit it as a test-crate root); `support/mod.rs` gained `read_request` / `respond`;
  `api_command_queue.rs` uses the platform's ECONNREFUSED (61 on macOS, 111 on Linux, 10061 on
  Windows) where the port hard-coded 10061. New: `module_support/mod.rs` (hand-written fake
  PMS API, install folder, gate adapter, supervisor rig), `fake_api.rs` (5),
  `module_lifecycle.rs` (4), `contract_drift.rs` (1), `tests/contract/*.json` (13 files copied
  from `apps/device-agent/contract/`).
- `README.md`: scope, config, behaviour, metrics / reports / commands, differences from .NET,
  switch-over steps, tests.

Elsewhere:

- `Cargo.toml` (workspace dependency), `Cargo.lock`.
- `crates/dl-agent-bin/Cargo.toml`, `src/main.rs`: `.module(dl_agent_pms_device::pms_device_module())`.
- `.cargo/config.toml`: `cargo wasm` excludes `dl-agent-pms-device` (uses `dl_http::Client` and
  `OffsetDateTime::now_utc`, absent on wasm; precedent 7c93224 for `dl-agent-einvoice`).
- `crates/dl-agent/README.md`: Modules table row. `CHANGELOG.md`: Unreleased → Added.

## Brief checklist

| Item | State |
|---|---|
| Crate, `pms_device_module() -> Factory`, registered in dl-agent-bin, opt-in | done |
| Service, .NET lifetime lines, service log file dropped; dl-agent logger, old messages | done |
| `appsettings.json` / `appsettings.{env}.json` from the old install dir; TokenFile, DataDirectory\outbox kept, old outbox results sent | done (`module_lifecycle` stop test resends an outbox entry on the next start) |
| Missing token / invalid config → degraded, no secret, wait on stop, slow re-check | done (30 s; tests use 200–500 ms hooks) |
| Running after the first claim round trip; degraded while the API keeps failing | done |
| Stop grace ≤ 5 s, blocked hardware call left on its thread, result in the outbox | done |
| Metrics claimed/ok/failed by kind, outbox pending, last command time | done |
| Reports `pms_device_api_failed`, `_auth_failed`, `_adapter_failed`, `_sdk_missing`; rate-limited, no payload / card data / token | done (adapter report keeps only the exception type) |
| LockSDK binding the only `unsafe` | done (`src/native`, `#![allow(unsafe_code)]` there only) |
| Builds for i686-pc-windows-gnu | done (clippy) |
| Builds for i686-win7-windows-gnu | **not checked locally** (see Not run) |
| Commands `pms-device.status`, `pms-device.flush_outbox`; no test tool | done |
| Unit tests and tests/*.rs ported | done (minus `host_run.rs`) |
| Contract copy + drift test on `PMS_DEVICE_CONTRACT_DIR` | done |
| Fake API: claim → simulated-lock → result; 204/404/409; never-taken result stays; raw-9100 printer gets the bytes | done (`fake_api.rs`) |
| Lifecycle: disabled by default; no token → degraded; prompt stop with a command in flight | done (`module_lifecycle.rs`) |
| Hand-written fakes only | yes |
| Files ≤ 800 lines | yes (largest `src/contracts/json.rs` 742, `tests/agent.rs` 726) |

## Checks (run in the worktree, through `~/.claude/scripts/heavy.sh`)

| Command | Result |
|---|---|
| `~/.cargo/bin/cargo fmt --all` then `cargo fmt --all -- --check` | clean |
| `~/.cargo/bin/cargo test -p dl-agent-pms-device -p dl-agent-bin` | 250 passed, 0 failed (62 unit + 188 integration; dl-agent-bin 0) |
| `PMS_DEVICE_CONTRACT_DIR=~/Projects/azolo-pms/apps/device-agent/contract cargo test -p dl-agent-pms-device --test contract_drift` | passed (compared, not skipped) |
| `~/.cargo/bin/cargo clippy -p dl-agent-pms-device -p dl-agent-bin --all-targets -- -D warnings` | clean |
| same with `--target x86_64-pc-windows-gnu` | clean |
| same with `--target i686-pc-windows-gnu` | clean |
| `RUSTDOCFLAGS="-D warnings" cargo doc -p dl-agent-pms-device --no-deps` | clean |
| `cargo build --workspace --exclude … --exclude dl-agent-pms-device --exclude dl-agent-pms … --target wasm32-unknown-unknown --profile min-size` | clean |

Timing in the new tests: every wait polls up to 20 s (`module_support::WAIT`); stop bounds are
5 s against a 500 ms test grace; the in-flight test waits on a gate, not on sleeps. The new
targets ran in 2.7–3.3 s each. Two full runs in this session, both green.

## Not run

- `e2e/run.ps1` of the port (side by side with the C# agent): not run, as the brief says; it
  needs Windows and the dotnet SDK.
- `i686-win7-windows-gnu`: tier 3, needs nightly with `-Zbuild-std`; no nightly toolchain is
  installed here. The crate adds no API beyond what `i686-pc-windows-gnu` checks (LockSDK through
  `dl_winos::dll`, std only), so the risk is low, but `scripts/build-dl-agent.sh` on the release
  machine is the first real build for that target.
- No hardware (lock encoders, serial / spooler / OPOS / PC/SC devices): covered only by the
  ported fakes. No Docker used.
- Full workspace suite and repo gates: left to the main session per the brief.

## Concerns

1. **`cargo wasm` is already red on main** because of `dl-agent-pms` (pms-site, PR #24):
   `dl_http::Client`, `dl_ws::ConnectOptions`, `dl_agent::tool` are not on wasm. This branch
   excludes only its own crate; `--exclude dl-agent-pms` in the alias would fix main. Left out
   to keep scope.
2. **Two flushes can overlap**: `pms-device.flush_outbox` runs beside the worker's own flush,
   and a worker abandoned after the grace can still be sending while the next cycle flushes.
   Each outbox file is sent at most once per pass. The API applies a repeated result with the
   same outcome again (`device-command.port.ts`: result handlers must be idempotent) and answers
   409 `DEVICE_OUTCOME_CONFLICT` only for a different outcome, so the worst case is a duplicate
   POST that re-runs an idempotent handler.
3. **Settings from the environment**: the old service read `Agent__*` and
   `DOTNET_ENVIRONMENT` from its own service environment; the module reads dl-agent's. A site
   that configured the old service through its environment needs `overrides` (README
   switch-over step 3).
4. **Local log lines** can contain device or API text, as in .NET; dl-agent does not ship its
   log, and reports carry no such text.
5. **Workspace layout**: a separate crate rather than part of `dl-agent-pms` (independent code
   and dependencies; `dl-agent-pms` is ~16.8k lines). Changing this later only moves files.
