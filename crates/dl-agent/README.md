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
  probation, restore the previous binary and blacklist the version.
- **Canary channel**: the hub puts demo tenants on `canary`; a build reaches `stable`
  only after the canary gate passes on the hub.
- **Self-healing**: each module runs on its own thread, panics are caught, restarts back
  off from 1 s to 5 min, and a module that stays degraded is reported.
- **Problem reports**: structured, fingerprinted, spooled on disk (500 max) and sent in
  batches when the hub is reachable.
- **Remote config**: applied per module with last-known-good; a module that fails within
  two minutes of a change reverts the config and reports `config_reverted`.

## Building a product agent

```rust,no_run
use std::sync::Arc;
use dl_agent::supervisor::{Module, ModuleContext};

struct Einvoice;

impl Module for Einvoice {
    fn name(&self) -> &str { "einvoice" }
    fn run(&mut self, ctx: &ModuleContext) -> Result<(), dl_agent::Error> {
        ctx.set_running("polling");
        while !ctx.stop().wait_timeout(std::time::Duration::from_secs(5)) {
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

## Commands

| Command | Meaning |
|---------|---------|
| `enroll --hub URL[,URL] --code CODE [--force]` | register with the hub |
| `run` | run in the console, or as the service when the SCM started it |
| `install` / `uninstall [--purge]` | system service |
| `status` | local state as JSON (no hub call) |

## Releasing

Build with the trusted keys baked in and the version stamped, sign offline, upload, publish:

```text
cargo run -p dl-agent --example release -- keygen release.seed rel-2026   # once; prints id:public
DL_AGENT_RELEASE_KEYS=rel-2026:<public> DL_AGENT_VERSION=1.2.0 cargo build --release -p dl-agent
cargo run -p dl-agent --example release -- sign release.seed rel-2026 dl-agent canary 1.2.0 \
    windows-x86_64 https://agent.pms.id.vn/v1/artifacts/dl-agent/1.2.0/windows-x86_64/dl-agent.exe \
    target/release/dl-agent.exe > canary.json
wrangler r2 object put agent-hub-artifacts/dl-agent/1.2.0/windows-x86_64/dl-agent.exe --file … --remote
curl -X PUT https://agent.pms.id.vn/admin/releases/dl-agent/1.2.0 --data-binary @canary.json …
```

Demo tenants (canary) take it first; `GET /admin/releases/dl-agent/1.2.0/gate` must pass before
`POST …/promote` with the same build signed for `stable`. Keep the seed off every server.

`--data DIR` (or `DL_AGENT_DATA`) picks the state directory; the default is
`%ProgramData%\dl-agent` or `/var/lib/dl-agent`.
