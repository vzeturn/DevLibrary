# Brief P2: `pms-device` module in dl-agent (RT-0164 task 557, step 2)

## Goal

The SMILE PMS device agent (service `SmilePmsDeviceAgent`, door-lock encoders and POS hardware on
front-desk workstations) moves onto the shared `dl-agent` as module `pms-device`, following
`crates/dl-agent-pms` (module `pms-site`, merged in PR #24; read its README and `src/module/`).
This slice builds the **module only**, inside DevLibrary. It does not change SMILE-PMS, release
anything, or deploy.

## Source to port

- `~/Projects/SMILE-PMS/apps/device-agent/rust/`: the Rust port of the .NET agent
  (`apps/device-agent/src/SmilePms.DeviceAgent`), about 12k lines, DevLibrary crates only (pinned to
  old rev `3b50836`). Read its README first. It claims commands (`POST /device-agent/commands/claim`),
  runs them through the adapters and posts results (`POST /device-agent/commands/{id}/result`), with
  the token file, the result outbox (.NET format) and `appsettings.json` (.NET configuration rules).
- The .NET agent is the reference whenever the two disagree.
- Contract files `apps/device-agent/contract/*.json`: copy them into the crate (`tests/contract/`),
  with a drift test against env `PMS_DEVICE_CONTRACT_DIR` (skip when unset), like `pms-site`'s
  manifest drift test.

## Target

- New crate `crates/dl-agent-pms-device` (or a `device` part of `dl-agent-pms` if that is clearly
  simpler; say which and why in the report), exporting `pms_device_module() -> Factory`, registered
  in `crates/dl-agent-bin/src/main.rs`. `Disabled` unless `modules.pms-device.enabled = true`.
  A machine may run both `pms-site` and `pms-device`, or only one.
- **Drop what dl-agent owns:** the `host` part (Windows service, .NET host lifetime lines, console
  log format/filters, the service log file). Logging goes through dl-agent's logger; keep the
  log *messages* where they help support, not the .NET formatting. Keep `config` (it reads the
  existing `appsettings.json`).
- **Keep the existing site state**, so a workstation running `SmilePmsDeviceAgent` switches in
  place: `appsettings.json` (+ `appsettings.{env}.json`) from the old install dir
  (`C:\Program Files\SmilePms\DeviceAgent` by default), `Agent:TokenFile`, `Agent:DataDirectory`
  and its `outbox\` (results the old agent left are sent).
- Module config `modules.pms-device`: `enabled`; `appsettings_dir` (optional, default above);
  `overrides` (optional object merged over the `Agent` section, e.g. `Adapter`, `Lock.LockKeyType`,
  so the hub can set them without editing files on the workstation).
- `Module::run`:
  - Missing token file / invalid config → `set_degraded` with the reason (no secret, no token)
    and wait on `ctx.stop()`; re-check on a slow timer so fixing the file needs no restart.
  - Otherwise run the claim loop; `set_running` after the first successful claim round trip;
    degraded while the API keeps failing, with the reason.
  - Stop promptly on `ctx.stop()`: finish the command in progress up to a short grace (≤ 5 s,
    hardware calls that block longer are left to finish on their own thread; the result stays in
    the outbox for the next start).
  - Metrics: commands claimed / ok / failed by kind, outbox pending, last command time.
  - Reports with stable codes: `pms_device_api_failed`, `pms_device_auth_failed`,
    `pms_device_adapter_failed`, `pms_device_sdk_missing` (rate-limited, no payload bodies, no
    card data, no tokens).
- **Native code:** the LockSDK binding (`native/`, dynamic `LoadLibraryExW` + `GetProcAddress`)
  stays the one place allowed `unsafe`, with `#[allow(unsafe_code)]` scoped to that module and a
  `// SAFETY:` note per block. It must build for dl-agent's x86 targets
  (`i686-pc-windows-gnu`, `i686-win7-windows-gnu`), not only `i686-pc-windows-msvc`; on x64 MFCARD
  answers DEVICE_SDK_NOT_INSTALLED as today. OPOS (IDispatch), PC/SC, serial, spooler and SAPI
  keep working on Windows; on Linux every hardware verb answers the same refusal as an absent
  device.
- `Agent:RunOnce` and console-only flags are not needed. Add a sub-command
  `dl-agent pms-device-test [--kind lock|pos] …` only if the port has an equivalent diagnostic;
  otherwise skip it.
- Remote commands, if cheap: `pms-device.status` (counters + adapter + lock type),
  `pms-device.flush_outbox`.

## Tests (mirror source structure, hand-written fakes, no mocking crates)

- Port the unit tests and `tests/*.rs` (contract, lock_contract, pos_contract, the C# cases).
- A fake PMS API on a local socket: claim → simulated-lock command → result; 204/404/409 answers;
  a result the API never takes stays in the outbox; a fake TCP printer (raw 9100) receives the
  expected bytes.
- Module lifecycle: disabled; no token → degraded; stop is prompt with a command in flight.
- The `e2e/run.ps1` side-by-side test needs Windows + dotnet: do not run it; say so in the report.

## Checks before commit

- `~/.cargo/bin/cargo fmt --all`
- `~/.cargo/bin/cargo test -p <crate> -p dl-agent-bin`
- `~/.cargo/bin/cargo clippy -p <crate> -p dl-agent-bin --all-targets -- -D warnings`, and the
  same with `--target x86_64-pc-windows-gnu` **and** `--target i686-pc-windows-gnu` (install the
  rustup target if missing).
- Every file ≤ 800 lines (200–400 typical). No swallowed errors.

## Rules

- Work only in the given worktree, branch `feat/dl-agent-pms-device`. Conventional Commits in
  English with the trailer below. **Do not push, merge or deploy.** Do not touch SMILE-PMS.
- Heavy builds through `~/.claude/scripts/heavy.sh <cmd>`. Don't dump logs. Remove any Docker
  container you create.
- Update `crates/dl-agent/README.md` (Modules table), `CHANGELOG.md` (Unreleased → Added) and the
  crate README (scope, config, differences from the .NET agent, switch-over steps).
- Report (≤ 10 lines) to `docs/plans/2026-11-19-pms-device-module-report.md`: status, SHA, one
  test line, gaps.

Commit trailer:

```
Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k
```
