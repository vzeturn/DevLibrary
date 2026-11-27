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
| `speaker` | `dl-agent-speaker` | PDAOrder staff alerts: joins the PDA order hub per RVC with the `pda-sync` enrolment and plays the RVC's tone and vi/en SAPI phrase for guest orders and service calls; no print relay. See its README. |

A module adds its own files to the `collect_diagnostics` bundle with
`ModuleContext::add_diagnostic_file` (one state file) and `add_diagnostic_folder` (a rolling
log folder, newest files first within a budget); both refuse names that suggest a secret and
everything is scrubbed (PROTOCOL.md §4.1).

Linux tests cover hub failover over loopback HTTP (`hub::failover_tests`) and the whole
`--fault probation` rollback trial (`agent::rollback_tests`, PROTOCOL.md §6.6). Only a
Windows canary machine checks the service manager restart between lives and the swap of a
running `.exe`.

The executable lives in its own package because product modules depend on this library.

## Installing on Windows

Create an enrolment code on the hub (`POST /admin/enroll-codes` answers `install_command`), then
on the site, in PowerShell as Administrator:

```text
irm https://agent.pms.id.vn/install.ps1 | iex
```

PowerShell 5.1+ (Windows 10 / Server 2016 and later). It asks for the enrolment code when none
is given. The SQL login is not asked for when the machine already has one that works (RT-0303):
the installer tries, best first, the login this agent saved, then each old agent's login (PMS
site `state\databases.json`, E-Invoice `config.enc`, Check-In `appsettings.Production.json`,
Membership `config.dpapi`, HotelLink `config.json`, PDAOrder SyncAgent `agent.json` + sealed
password; in place, renamed `.bench-old-*` by the bench, or where a bench record moved them;
DPAPI LocalMachine; a secret sealed for one Windows user (DPAPI CurrentUser) is skipped with a
warning, since the service could never open it), then Windows sign-in to the local SQL Server. The first that signs in to
every SMILE database is saved with `login_source` (shown by `selftest` and sent in the survey).
Only when none works does it show the menu ([1] Windows, [2] SQL; server, user and a hidden
password); an unattended run then installs and enrols without SQL, files one
`sql_login_not_found` report listing each login tried and why it failed, and exits 1. SRBackup
keeps no SQL login on the machine (the hub hands it one), so it has nothing to reuse.
Options go through the scriptblock form:

```text
& ([scriptblock]::Create((irm https://agent.pms.id.vn/install.ps1))) canary -Code XXXX-XXXX -SqlPrompt
```

PowerShell 2.0 (Windows 7 / Server 2008 R2):

```text
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072; & ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('https://agent.pms.id.vn/install.ps1'))) -Code 'XXXX-XXXX'
```

The same install line works for a second product: on a machine already enrolled (for example
for Check-In), running the install line of another product (for example PDAOrder) keeps the
agent's identity and its modules and attaches the new product's code to the same agent
(`Attached pda-sync to this agent`). By hand: `dl-agent attach --code XXXX-XXXX`. Only
`-Reenroll` replaces the identity. See PROTOCOL.md §2.3.

Unattended (RMM, SYSTEM): environment only, never a password on the command line:
`DL_AGENT_ENROLL_CODE`, `DL_AGENT_CHANNEL`, `DL_AGENT_SQL_AUTH` (`windows`|`sql`),
`DL_AGENT_SQL_SERVER`, `DL_AGENT_SQL_USER`, `DL_AGENT_SQL_PASSWORD`. Exit codes: 0 ok, 1 failed,
65 build failed verification, 69 hub unreachable, 77 not Administrator, 78 configuration.

The script only downloads, checks and hands off: size and SHA-256 against `/install/info`, then
`verify-self` checks the Ed25519 release signature of `/install/manifest/...` (the installed
agent does it when it knows the verb, else the download itself), then `<download> install --hub
...` does the machine setup: Defender exclusion only when Defender is the active antivirus (with another
one, such as Kaspersky, it prints what to trust instead; the script passes `--no-defender` and adds
the exclusion itself), older copies and `-Replace` services removed,
program placed in `%ProgramFiles%\DevLibrary\<product>`, enrol unless already enrolled, SQL login,
`-Setup`, service + watchdog task, wait for the first heartbeat. Re-running the line repairs or
updates the install and keeps the identity. A build older than `dl-agent install` is installed by
the previous script (`/install/legacy.ps1`) with the same options.

The first argument picks what the machine follows: `stable` / `canary` (a local choice that wins
over the hub's assignment), `auto` (the hub's channel again) or a version `x.y.z` (held there).
`-Channel canary` only picks the build installed now. `-SqlPrompt` asks for the SQL login when SQL
Server is on another machine. See `services/agent-hub/README.md` for the hub side.

### Demo bench: one command, every module (RT-0261)

For a test PC wired to each product's **demo** tenant. On the hub (admin auth like every `/admin`
route):

```sh
curl -sS https://agent.pms.id.vn/admin/demo-bench -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"tenant_id": "demo-bench", "site_id": "owner-pc", "settings": {"DL_AGENT_DEMO_EINVOICE_URL": "https://…"}}'
```

It answers a one-time enrolment code (canary: the tenant is created as, or must already be, a demo
tenant; `409 tenant_not_demo` otherwise) whose first config enables `einvoice`, `pms-site`,
`arrival`, `membership`, `hotellink`, `backup`, `pda-sync`, `pms-device` and `speaker` (or the
`modules` asked for), the `install_command` with `-Demo`, the variables each module needs and
`operator_env`: PowerShell lines with the non-secret `settings` prefilled and a placeholder for each
module code. The hub mints only the dl-agent code: every module code comes from that product's own
cloud and never passes through the hub (`settings` refuses them). On the PC, as Administrator, set
the module codes, then run the line:

```text
$env:DL_AGENT_DEMO_EINVOICE_TOKEN = '…'   # one line per module, from operator_env
$env:DL_AGENT_DEMO_PDA_CODE = '…'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072; & ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('https://agent.pms.id.vn/install.ps1'))) -Code 'XXXX-XXXX' -Demo
```

(Or `$env:DL_AGENT_DEMO = '1'; $env:DL_AGENT_ENROLL_CODE = 'XXXX-XXXX'` and the short
`irm https://agent.pms.id.vn/install.ps1 | iex`.) The values may also go in `-DemoEnv @{ NAME =
'value' }` (they then sit in the console history); `-DemoModules einvoice,pda-sync` (or
`DL_AGENT_DEMO_MODULES`) picks modules. `-Demo` installs canary (another target or `-Channel` is
refused) and, after the install:

| Module | Enrol tool | Operator variables (secret → the variable the tool reads; options → flag) |
|---|---|---|
| `einvoice` | `einvoice-enroll` | `DL_AGENT_DEMO_EINVOICE_TOKEN` → `DL_AGENT_EINVOICE_TOKEN`; `DL_AGENT_DEMO_EINVOICE_URL` → `--platform-url` (SQL login from `MMEI_SQL_*` when set) |
| `pms-site` | `pms-enroll` | `DL_AGENT_DEMO_PMS_CODE` → `DL_AGENT_PMS_CODE`; `DL_AGENT_DEMO_PMS_URL` → `--cloud-url` |
| `arrival` | `arrival-enroll` | `DL_AGENT_DEMO_ARRIVAL_TOKEN` → `DL_AGENT_ARRIVAL_TOKEN`, optional `DL_AGENT_DEMO_ARRIVAL_PMS` → `DL_AGENT_ARRIVAL_PMS`; `DL_AGENT_DEMO_ARRIVAL_URL` → `--cloud-url` |
| `membership` | `membership-enroll` | `DL_AGENT_DEMO_MEMBERSHIP_TOKEN` → `DL_AGENT_MEMBERSHIP_TOKEN`, optional `DL_AGENT_DEMO_MEMBERSHIP_SMILE` → `DL_AGENT_MEMBERSHIP_SMILE`; `DL_AGENT_DEMO_MEMBERSHIP_URL` → `--hub-url` |
| `hotellink` | `hl-enroll` | `DL_AGENT_DEMO_HL_TOKEN` → `DL_AGENT_HL_TOKEN`, optional `DL_AGENT_DEMO_HL_SQL_PASSWORD` → `DL_AGENT_HL_SQL_PASSWORD`; `DL_AGENT_DEMO_HL_URL` → `--hub-url`, `DL_AGENT_DEMO_HL_HOTEL_ID` → `--hotel-id`, `DL_AGENT_DEMO_HL_INSTANCE_ID` → `--instance-id`, optional `DL_AGENT_DEMO_HL_SQL_USER` → `--sql-user` |
| `backup` | `backup-enroll` | `DL_AGENT_DEMO_BACKUP_KEY` → `DL_AGENT_BACKUP_KEY`; `DL_AGENT_DEMO_BACKUP_URL` → `--hub-url` |
| `pda-sync` | `pda-enroll` | `DL_AGENT_DEMO_PDA_CODE` → `DL_AGENT_PDA_CODE`; `DL_AGENT_DEMO_PDA_URL` → `--url` |
| `pms-device` | `pms-device-enroll` | `DL_AGENT_DEMO_PMS_DEVICE_URL` → `--api-url`, optional `DL_AGENT_DEMO_PMS_DEVICE_AGENT_ID` → `--agent-id`, optional `DL_AGENT_DEMO_PMS_DEVICE_TOKEN` → `DL_AGENT_PMS_DEVICE_TOKEN` (else generated; the tool prints its sha256 to register in the PMS) |
| `speaker` | none | uses the `pda-sync` identity |

1. each enrol tool runs with its code set for that process only (never argv, never written by the
   script; agent output lines are masked); a module without its required variables is `skip`
   (`set …`), a failed one does not stop the next;
2. the service is restarted and the script waits up to 90 s for a hub contact;
3. `dl-agent selftest --trigger install` runs (and uploads), and a table prints one row per
   module: `Enrol` (`ok`, `skip`, `n/a`, `FAILED`) and `Selftest` (the worst of its checks:
   `pass`, `warn`, `fail`, `skip`, or `none`);
4. every `DL_AGENT_DEMO_*` variable and every tool variable is removed from the session and
   `-DemoEnv` is emptied, whatever happened. Exit code 1 (unattended runs) when a module failed.

The table lives once in `services/agent-hub/src/domain/demo.ts`; a test checks each entry against
the tool's own source (sub-command, variable, flags). The `/bench/<token>` bundle (hub README
"Bench (test PC)") stays for benches that also restore a SMILE copy and run SQL fixups.

Remove: `irm https://agent.pms.id.vn/uninstall.ps1 | iex` (or `dl-agent uninstall`): tells the
hub, removes the watchdog task, the service, the program and the Defender exclusion; the data
folder (identity, spool, logs) stays unless `-Purge` (`--purge`).

### Attach a product from the hub, no site visit (RT-0261)

Install dl-agent once per customer (any product's install line). Every further product is
switched on from the hub; nobody touches the machine again:

```sh
curl -sS -X POST https://agent.pms.id.vn/admin/agents/<agent_id>/attach \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
  -d '{"module":"pms-site","secret":{"DL_AGENT_PMS_CODE":"<one-time code from the PMS console>"},
       "options":{"--cloud-url":"https://<pms cloud>"},"external_id":"azolo-demo"}'
```

The hub enables the module, sends the agent one `attach` command that runs the module's enrol
tool with the code (in a variable, never on the command line; the hub forgets it once the
agent has it), then restarts the module. `GET /admin/commands?agent_id=<id>` shows the tool's
exit and output (secrets masked). Variables and flags per module are the demo bench table
above; `speaker` takes only `{"module": …}`. For einvoice, arrival, membership, pms-site and
pms-device the hub also passes what the agent config names for the module
(`modules.einvoice.config_dir`, `modules.arrival.install_dir`, `modules.membership.program_data_dir`,
`modules.pms-site.config_path` → `--config` and `state_dir` → `--state-dir`,
`modules.pms-device.appsettings_dir` → `--appsettings-dir`),
so the tool writes where the module reads; `reenroll: true` with a new `external_id` also drops the
agent's older links of that product; with no such key the tool's own default applies.
`arrival-enroll` with no connection string given, saved or left by the old agent uses the
`SMILE_FO` entry of `sql.json` that `dl-agent sql-setup` saved. Needs dl-agent 1.3.17 or newer.

Two more fields cover a machine that is already enrolled elsewhere or whose service account is
not a SQL Server administrator (agent 1.3.19 or newer for `--sql-admin-saved`):

- `"reenroll": true` enrols again over an existing enrolment, moving the module to the new
  product tenant (the hub adds the tool's `--force`; einvoice, pms-site, arrival, membership,
  hotellink, backup). The SQL connection the module already has is kept.
- `"flags": [...]` passes value-less flags from the module's list. `pda-sync` takes
  `--sql-admin-saved`: `pda-enroll` signs in as the SQL login dl-agent keeps in `sql.json`
  (the POS database's, else the first SQL login) instead of LocalSystem, which fixes
  `SQL_NO_ACCESS ... error 15247` with no site visit. With no administrator given at all,
  `pda-enroll` tries Windows first, then the SMILE default login `smile` (password from
  `"secret": {"DL_AGENT_PDA_SQL_DEFAULT_PASSWORD": "…"}`, else the one built into the release). Or give the administrator explicitly:
  `"options": {"--sql-admin-user": "sa"}` with
  `"secret": {"DL_AGENT_PDA_SQL_ADMIN_PASSWORD": "…"}`.

```sh
# Move einvoice to the azolo-demo tenant; enrol pda-sync with the saved SQL login.
-d '{"module":"einvoice","secret":{"DL_AGENT_EINVOICE_TOKEN":"<token>"},
     "options":{"--platform-url":"https://mm-einvoice.fly.dev"},"reenroll":true}'
-d '{"module":"pda-sync","secret":{"DL_AGENT_PDA_CODE":"<code>"},
     "options":{"--url":"https://azolo-demo-pos.pms.id.vn"},"flags":["--sql-admin-saved"]}'
```

Refusals: `unknown_flag` (not in the module's list), `reenroll_not_supported` (pda-sync has no
re-enrol flag), `module_has_no_enrol_step` (speaker).

### Machines that already run an agent: no install line again (RT-0261)

**dl-agent already installed.** Nothing to run on site. The agent updates itself from the hub
(signed manifest, probation, automatic rollback; PROTOCOL.md §5–6): canary machines take a new
build within one heartbeat, stable machines once the build is promoted (`rel.sh promote`, after
the canary gate). A patch reaches every stable agent at once inside its site window, a minor
ramps 10 → 50 → 100 %, a major only where allowed (ADR 0001). Remote attach needs 1.3.17 or
newer; older agents get it through that same update, then `attach` works.

**An old product agent (before dl-agent).** The product's own cloud orders its old agent to
replace itself: the old agent asks its cloud for the dl-agent install script with a one-time
hub code minted by the product key, starts it detached as SYSTEM, the script enrols dl-agent
with the product's enrol tool (`--switch`: keeps the hotel identity, SQL login and queue) and
`-Replace` removes the old service only after the enrol succeeded. Nobody runs a command.

| Old agent | Ordered from its cloud, no site visit | Where |
|-----------|----------------------------------------|-------|
| E-Invoice (Go) | yes: `agent_config.switch_to_dl` (RT-0237) | MM-EINVOICE `internal/agentcollector/switch_dl.go`, `POST /api/v1/agent/update/switch` |
| Check-In (.NET) | yes: `POST /api/platform/tenants/{id}/dl-switch` (RT-0252) | SmileCheckin `docs/saas/AGENT_DL_INSTALLER.md` |
| Hotel Link (Go) | no: new installs only (`SMILEHL_AGENT_INSTALLER=dl`) | SMILE-HLAPI `go/internal/hub/dlinstall.go` |
| Membership (.NET) | no: `membership-enroll --switch` exists, nothing orders it | DevLibrary `crates/dl-agent-membership/src/enroll/switch.rs` |
| PDAOrder SyncAgent / speaker | no | SML-PDAOrder `apps/sql-sync-agent`, `apps/speaker-agent` |
| SMILE-PMS site / device agent | no | SMILE-PMS `apps/site-agent`, `apps/device-agent` |
| SR Backup (Go) | no | SRBackup `agent/` |

Each "no" row is work in that product's repository (its old agent learns the switch order, as
E-Invoice and Check-In did, and ships through the old agent's own self-update); the hub side
(product keys, one-time codes, `install.ps1 -Setup <tool> -Replace <service>`) already serves
every product.

## Commands

| Command | Meaning |
|---------|---------|
| `--version` | print the version |
| `enroll --hub URL[,URL] --code CODE [--force]` | register with the hub |
| `run` | run in the console, or as the service when the SCM started it |
| `install --hub URL[,URL] [stable\|canary\|auto\|x.y.z] [...]` | machine setup (what the one-line installer runs); prompts mask the password |
| `install stable\|canary\|auto` | follow that channel (`auto`: the hub's) and update now; removes a pin |
| `install x.y.z` | install that signed version and hold it (heartbeat `update_pin`) |
| `install` | (re)register the service for this binary |
| `update [--wait S]` | check the agent's channel and install a newer signed build now; prints from and to |
| `verify-self --manifest F [--file EXE] [--target T] [--product P]` | check a build against its signed manifest; exit 65 when it does not match |
| `uninstall [--purge] [--keep-files]` | remove the agent (see above); `--keep-files` keeps the program for an installer |
| `status` | local state as JSON (no hub call) |
| `sql-add NAME --connection CS [--max N]` | declare a database (or env `DL_AGENT_SQL_CONNECTION`); restart to apply |
| `sql-test [NAME]` | connect to every (or one) declared database; exit 1 if any fails |

Automatic updates stop with `DL_AGENT_DISABLE_AUTOUPDATE=1` in the service's environment or a
`no-update` file in the data folder; `update` and the hub's `update_now` still install
(PROTOCOL.md §6.5).

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

### Release without the iMac: one command, one-shot Fly build machine (RT-0311)

`scripts/release/` releases from any machine with the repo, `curl` and `cargo` — a cloud
session included — with no Docker daemon and no machine-specific path:

```text
export FLY_API_TOKEN=…           # deploy token of the Fly app dl-xbuild (FLY_BUILD_APP)
export CLOUDFLARE_API_TOKEN=…    # R2 edit on the hub's account
export DL_AGENT_RELEASE_SEED=…   # release seed (base64), or DL_AGENT_RELEASE_SEED_FILE
export AGENT_HUB_ADMIN_TOKEN=… AGENT_HUB_ACCESS_CLIENT_ID=… AGENT_HUB_ACCESS_CLIENT_SECRET=…
export MMEI_BUILD_DEFAULT_SQL_PASSWORD=…   # optional, compiled into einvoice-enroll
scripts/release/canary.sh 1.3.17           # build + sign + upload + publish canary
scripts/release/rel.sh gate 1.3.17         # later: the canary gate
scripts/release/rel.sh promote 1.3.17      # once the gate passes (stable manifest signed already)
```

`canary.sh` runs, in order: `fly-build.sh` creates a Fly Machine (`auto_destroy`, restart
`no`, `performance-8x`, image `rust:1-bookworm` or `FLY_BUILD_IMAGE`) through the Machines API;
the machine downloads the pushed commit (`DL_BUILD_REF`, default `HEAD`; push first), installs
the toolchain with `scripts/xbuild/setup.sh` (the same script the Dockerfile runs), runs
`DL_XBUILD_NATIVE=1 scripts/build-dl-agent.sh` (same DLL, glibc and wine checks), stages the
unsigned binaries, log and exit status in R2 `dl-agent-builds/<version>/` and exits, and Fly
removes it. `fly-build.sh` waits (default 90 min), prints the log tail and destroys the machine
itself on timeout, error or Ctrl-C, so none is left running. Then `rel.sh fetch` downloads the
staged binaries, `rel.sh sign` signs canary and stable manifests here (the seed is written to a
mode-600 temp file only for the signer and removed; it never reaches the build machine),
`rel.sh upload` puts the artifacts at `dl-agent/<version>/<target>/` and `rel.sh canary`
publishes. Every credential is read from the environment and handed to `curl` through a
process-substitution config, never argv; the machine sees the Cloudflare token for staging
only while it lives. `FLY_BUILD_DRY_RUN=1 scripts/release/fly-build.sh <version>` prints the
machine config with secrets masked.

**Warm build machine (RT-1340).** One-time: `scripts/release/warm-setup.sh` builds the toolchain image
on Fly's remote builder, creates the 30 GB volume `dlxcache` and the stopped machine `dl-xbuild-warm`
(re-run it after `scripts/xbuild/` changes; the volume is kept). `fly-build.sh` then uses it
automatically: it updates the machine's environment, starts it, waits for it to stop and stops it
again (so only the volume is billed between releases), and clears the Cloudflare token from the
machine config. The machine keeps a git checkout in `/cache/src` (unchanged files keep their mtimes,
so cargo recompiles only what changed), `/cache/registry`, `/cache/git` and `/cache/target`.
`FLY_BUILD_MODE=oneshot` forces the old create-and-destroy path. Measured with all five targets at one
commit: one-shot 824 s, warm cold cache 741 s, warm filled cache 29 s. The iMac native build
(`scripts/build-dl-agent.sh` with `DL_XBUILD_NATIVE=1`) is not an option here: the Windows and Win7
targets need mingw and wine, which the iMac lacks.

`--data DIR` (or `DL_AGENT_DATA`) picks the state directory; the default is
`%ProgramData%\dl-agent` or `/var/lib/dl-agent`.
