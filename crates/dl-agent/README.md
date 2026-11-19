# dl-agent

The shared on-prem agent runtime of DevLibrary. One process per machine runs the product
modules under a supervisor, keeps itself up to date from the hub over a signed canary →
stable channel, rolls back a bad build on its own, reports problems to the cloud and takes
remote config and commands. The wire contract is [`PROTOCOL.md`](PROTOCOL.md); the
inventory of the agents it replaces is [`INVENTORY.md`](INVENTORY.md).

No dependencies outside DevLibrary, no `unsafe`.

## Replaces

| Project agent | What moves into dl-agent |
|---------------|--------------------------|
| PDAOrder site agent, SMILE-PMS on-prem agent, MMEI agent, wifi-hub router agent | service install, enrolment, hub heartbeat, signed self-update, rollback, problem reports, remote config, commands |

## What it does

- **Service**: `install` registers a Windows service (restart recovery actions, delayed
  auto-start) or a systemd unit (`Restart=always`). Exit code 75 asks the manager for a
  restart after an update.
- **Enrolment**: `enroll --hub URL --code CODE` creates an Ed25519 key pair; every later
  request is signed (`X-Agent-*` headers), with clock-skew correction and hub fallback.
- **Signed self-update**: the release manifest is verified against keys built into the
  binary (`DL_AGENT_RELEASE_KEYS="id:base64"` at compile time), the artifact against its
  SHA-256; the new build runs on probation and is committed only when every module runs
  and the hub answers. Three boots without commit, or a module unhealthy through the whole
  probation, restore the previous binary and blacklist the version. Modules already
  unhealthy when the update was installed do not count.
- **Canary channel**: the hub puts demo tenants on `canary`; a build reaches `stable`
  only after the canary gate passes on the hub.
- **Self-healing**: each module runs on its own thread, panics are caught, restarts back
  off from 1 s to 5 min, and a module that stays degraded is reported.
- **Problem reports**: structured, fingerprinted, spooled on disk (500 max) and sent in
  batches when the hub is reachable.
- **Remote config**: applied per module with last-known-good; a module that fails within
  two minutes of a change reverts the config and reports `config_reverted`.

- **SQL Server bridge**: one pool per database declared in `<data>/sql.json` (connection
  strings stay on the machine, mode 0600), shared by every module through
  `ctx.sql("SMILE_FO")`. Sessions are reused and health-checked, a down database trips a
  breaker with backoff instead of hammering it, every query has a timeout (15 s) and lock waits
  are capped (8 s), old servers are reached by stepping down TLS 1.2 → TLS 1.0 → unencrypted
  (reported), and a built-in
  `sql` module reports outages, surveys each database (engine, compat level, collations,
  grants; PROTOCOL.md §9.5) and answers `sql.ping` / `sql.status` / `sql.survey`. The hub may tune the
  limits (`body.sql`) but never the connection. See PROTOCOL.md §9.

## Building a product agent

```rust,no_run
use std::sync::Arc;
use dl_agent::supervisor::{Module, ModuleContext};

struct Einvoice;

impl Module for Einvoice {
    fn name(&self) -> &str { "einvoice" }
    fn run(&mut self, ctx: &ModuleContext) -> Result<(), dl_agent::Error> {
        // Shared pool for a database declared on the site (`dl-agent sql-add SMILE_FO …`).
        let fo = ctx.sql("SMILE_FO")?;
        ctx.set_running("polling");
        while !ctx.stop().wait_timeout(std::time::Duration::from_secs(5)) {
            let mut c = fo.get()?; // returned to the pool when dropped
            let _rows = c.query("SELECT TOP 1 1 AS one", &[]);
            drop(c);
            while let Some(cmd) = ctx.next_command() {
                ctx.complete(&cmd.id, true, "done");
            }
        }
        Ok(())
    }
}

fn main() {
    let app = dl_agent::cli::App::new("azolo-agent", "Azolo Agent")
        // Release line on the hub and the version this build reports.
        .release("azolo-agent", env!("CARGO_PKG_VERSION").parse().unwrap())
        .module(Arc::new(|| Box::new(Einvoice)));
    std::process::exit(dl_agent::cli::main(app));
}
```

A module can also add a sub-command for a setup step (`App::tool(dl_agent::tool::Tool { name,
grammar, run })`); `--data` is added to it. `install.ps1 -Setup <name>` runs it after enrolment,
with secrets passed in env `DL_AGENT_SETUP_ARGS` (appended to that sub-command's arguments only,
then removed), and `-Replace '<service pattern>'` removes the product's previous agent after it.

## Modules

The released `dl-agent` executable (package `dl-agent-bin`) registers every product module;
each product module is opt-in and stays `disabled` until the hub's remote config sets
`modules.<name>.enabled = true` (PROTOCOL.md §8). A module opts in by returning `false` from
`Module::enabled_by_default`.

| Module | Crate | What it does |
|---|---|---|
| `sys`, `sql` | `dl-agent` | Built in, on by default: host facts, SQL Server bridge. |
| `einvoice` | `dl-agent-einvoice` | Azolo E-Invoice: collects SMILE_FO / SMILE_POS bills, pushes them to the platform, writes invoice numbers back, relays VNPT portal calls. Reads the Go agent's `config.enc`; see its README. |
| `membership` | `dl-agent-membership` | Azolo Membership (read side of `MM.Membership.Agent`): heartbeat with the SMILE identity, sync-config, availability, room inventory, room types and folio observations to the Membership hub; jobs counted, not executed yet. Reads the .NET installer's `config.dpapi`; sub-command `membership-enroll`. See its README. |
| `pms-site` | `dl-agent-pms` | SMILE PMS site agent: enrolls, holds the stream to the PMS cloud, runs allow-listed ops and signed migrations on the hotel's SQL Server. Uses `SmilePmsAgent`'s config and state, so a site switches without re-enrolling; sub-commands `pms-enroll`, `pms-migrate`. See its README. |
| `pms-device` | `dl-agent-pms-device` | SMILE PMS device agent: claims device commands from the PMS API, drives door-lock encoders (LockSDK on the x86 build) and POS hardware (raw-TCP and serial printers, spooler, card readers, OPOS, speech), posts the results through an outbox. Reads `SmilePmsDeviceAgent`'s appsettings, token file and outbox where they are; commands `pms-device.status`, `pms-device.flush_outbox`. See its README. |
| `arrival` | `dl-agent-arrival` | Azolo Check-In site agent: polls the Check-In cloud outbound only over sealed envelopes, answers its requests from the hotel's SMILE PMS through the fixed `srCI_*` operation catalog, keeps the legal vault and the idempotency journal, installs the procedure bundle. Reads `MM.Arrival.Agent`'s appsettings, identity, tenant key, vault and journal where they are; sub-command `arrival-enroll`. See its README. |

The executable lives in its own package because product modules depend on this library.

## Installing on Windows

Create an enrolment code on the hub (`POST /admin/enroll-codes` answers `install_command`), then
on the site run in PowerShell as Administrator (PowerShell 2.0 / Windows 7 included):

```text
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072; & ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('https://agent.pms.id.vn/install.ps1'))) -Code 'XXXX-XXXX'
```

Add `-SqlPrompt` when SQL Server is on another machine (asked and tested before the code is
spent), `-Channel canary` for demo sites. Re-running the same line upgrades or repairs the
install and keeps the identity. See `services/agent-hub/README.md` for every step.

## Commands

| Command | Meaning |
|---------|---------|
| `enroll --hub URL[,URL] --code CODE [--force]` | register with the hub |
| `run` | run in the console, or as the service when the SCM started it |
| `install` / `uninstall [--purge]` | system service |
| `status` | local state as JSON (no hub call) |
| `sql-add NAME --connection CS [--max N]` | declare a database (or env `DL_AGENT_SQL_CONNECTION`); restart to apply |
| `sql-test [NAME]` | connect to every (or one) declared database; exit 1 if any fails |

## Releasing

Build with the trusted keys baked in and the version stamped, sign offline, upload, publish:

```text
cargo run -p dl-agent --example release -- keygen release.seed rel-2026   # once; prints id:public
DL_AGENT_RELEASE_KEYS=rel-2026:<public> DL_AGENT_VERSION=1.2.0 cargo build --release -p dl-agent-bin
cargo run -p dl-agent --example release -- sign release.seed rel-2026 dl-agent canary 1.2.0 \
    windows-x86_64,windows7-x86_64 \
    https://agent.pms.id.vn/v1/artifacts/dl-agent/1.2.0/windows-x86_64/dl-agent.exe,https://agent.pms.id.vn/v1/artifacts/dl-agent/1.2.0/windows7-x86_64/dl-agent.exe \
    dist/windows-x86_64/dl-agent.exe,dist/windows7-x86_64/dl-agent.exe > canary.json
wrangler r2 object put agent-hub-artifacts/dl-agent/1.2.0/windows-x86_64/dl-agent.exe --file … --remote
curl -X PUT https://agent.pms.id.vn/admin/releases/dl-agent/1.2.0 --data-binary @canary.json …
```

Site binaries come from `scripts/build-dl-agent.sh <version>` (Docker, image from
`scripts/xbuild/`): `linux-x86_64`, `windows-x86_64` and `windows-x86` (Windows 10 / Server
2016+), `windows7-x86_64` and `windows7-x86` (Windows 7, Server 2008 R2 and 2012; tier-3
`*-win7-windows-gnu` targets on nightly with `build-std`). Windows builds are static and may
import only system DLLs (the script fails otherwise); the Win7 builds must not import
`bcryptprimitives.dll`, and the Win7 x64 build is run under wine. A Win7 build reports the
target `windows7-<arch>`, so the hub hands it only Win7 artifacts. Publish one artifact per target.

Demo tenants (canary) take it first; `GET /admin/releases/dl-agent/1.2.0/gate` must pass before
`POST …/promote` with the same build signed for `stable`. Keep the seed off every server.

`--data DIR` (or `DL_AGENT_DATA`) picks the state directory; the default is
`%ProgramData%\dl-agent` or `/var/lib/dl-agent`.
