# Report: dl-agent self-test (brief ST, RT-0164)

Branch `feat/dl-agent-selftest` (worktree `~/Projects/DevLibrary-st`), based on `origin/main` 11ac477.
Status: **DONE_WITH_CONCERNS**. Every gate is green. Nothing was run on Windows: there is no Windows machine here.

## Commits

- `366ed85` `chore: merge main into feat/dl-agent-selftest`. Main (a304141) had moved on with the membership M2 write side (50 files in `dl-agent-membership`) and the hub's `POST /v1/agent/selftest` and bench bundle. No file overlapped this work. The membership self-test needed the M2 settings (`write`, `pull_jobs`). The upload contract was checked against main's `schemas.ts`.
- The feature commit (`feat(dl-agent): selftest ...`) carries this report and the brief.

## What changed

**`dl-agent` (library)**

- `selftest/` (new):
  - `mod.rs`: the contract (`Check`, `Outcome`, `Status`, `SelfTest`, `Run`, `Trigger`, caps), with detail ≤ 600, remedy ≤ 300, id ≤ 100 and title ≤ 300 characters, ≤ 500 checks and ≤ 250 KB per body.
  - `sanitize.rs`: masks `password=`, `pwd=`, `token`, `secret`, `key=`, bearer/basic credentials, URL userinfo, query secrets and long base64/hex blobs.
  - `runner.rs`: `bounded`, plus per-module threads with `MODULE_TIMEOUT` 120 s and panic capture.
  - `checks_core.rs`, `checks_hub.rs`, `legacy.rs`: the checks.
  - `execute.rs`: one run for the CLI and the command.
  - `cli.rs`: the table output.
  - `tests.rs`: the tests.
- `cli.rs`:
  - `App::selftest`.
  - The `selftest` sub-command: `--data`, `--module`, `--json`, `--no-upload`, `--trigger cli|install`.
  - Exit code 1 on any `fail`.
- `sys_module.rs`: remote command `selftest` through a `SelfTestHook` that the CLI installs, so it runs the same checks in the service.
- `hub.rs`: `HubClient::selftest`, a signed `POST /v1/agent/selftest`. A 404 returns `Ok(false)`, which is logged and ignored.
- `agent.rs`: wires the hook.
- `manifest.rs`, `watchdog.rs`, `service_watch.rs`: small read-only accessors for the checks (manifest verify without install, beat reader, recovery-action query).

**Modules.** Each gets a new `module/selftest.rs`, re-exported from `lib.rs`.

| Crate | Export | Supporting change |
|---|---|---|
| `dl-agent-einvoice` | `einvoice_selftest` | `sql::missing_procs` |
| `dl-agent-pms` | `pms_site_selftest` | — |
| `dl-agent-pms-device` | `pms_device_selftest` | — |
| `dl-agent-hotellink` | `hotellink_selftest` | `config::read`: decrypts without the plaintext-to-DPAPI rewrite that `load` does |
| `dl-agent-backup` | `backup_selftest` | `SqlConn::can_backup`, using `HAS_PERMS_BY_NAME(db,'DATABASE','BACKUP DATABASE')`; the default trait impl returns `Some(true)` for fakes |
| `dl-agent-arrival` | `arrival_selftest` | `TenantKeyStore::load_existing` (read-only); `load_or_create` now uses it |
| `dl-agent-membership` | `membership_selftest` | — |

**`dl-agent-bin`.** Registers all seven module self-tests next to their modules.

**`CHANGELOG.md`.** New entry under Unreleased / Added.

## Checks

The ids are stable. Every module first reads its own configured folder, and reports `skip` ("not enrolled: …", naming the folder) when the config is absent. It never reports `fail` for that.

### core

| id | What |
|---|---|
| `identity` | `identity.json` present; product, version and agent id |
| `version` | binary version and the channel from the hub (or the last beat) |
| `permissions` | `identity.key` and `sql.json` restricted (SYSTEM + Administrators on Windows, 600 on Unix) and the data dir not open to others. `skip` before enrollment |
| `service` | Windows service installed and running, with recovery restarts after 5 s, 10 s and 60 s. `skip` off Windows |
| `watchdog` | watchdog scheduled task present. `skip` off Windows |
| `hub` | hub reachable and signed heartbeat accepted. A fresh service beat (≤ 600 s) counts. If the service was just started, waits up to 45 s for its beat. Otherwise the CLI sends one signed heartbeat itself |
| `clock` | skew against the hub's `Date`/`now`: `warn` at > 60 s, `fail` at > 300 s |
| `update` | manifest for the channel fetched and its signature verified (no install) |
| `spool` | `reports.json` writable, and flushable when the hub is reachable |
| `sql`, `sql.<name>` | every DB in `sql.json`: connect, survey warnings (C3; site-only notes such as Express or an old engine are shown, not warned), and the optional `{requirement, database}` schema probe sent with the command (C4) |
| `legacy.services` | see the addendum below |

### einvoice

- `config`: `config.enc` in `modules.einvoice.config_dir` decrypts.
- `platform`: an auth ping with the agent key.
- `sql.fo` and `sql.pos`: FO/POS have the collector procedures (`missing_procs`). `sql.pos` is `skip` when POS is not collected.
- `pending.<source>`: one collector read, reporting the count only.

### pms-site

- `config`: the module's `config_path` and `state_dir`. The enrollment code is still waiting → `warn`.
- `cloud`: mints a token with the agent's signed key. `skip` before enrollment. It never creates `agent.key`.
- `databases`, `db.<name>`: `databases.json` present, and `SELECT 1` on each DB.
- `op`: runs `pos.checkVersion.get` locally through `SqlExecutor`. Both applied and rejected count as a pass.

### pms-device

- `config`: `appsettings*.json` in `appsettings_dir` bind and validate, and the token file reads.
- `api`: an unauthenticated GET of `ApiBaseUrl`. Any HTTP answer is a pass. Nothing is claimed.

### hotellink

- `config`: decrypts at `config_path` (DPAPI) without being rewritten. A template or placeholder config → `skip`.
- `hub`: `GET /api/agent/config` with the agent token.
- `queue`: `queue.jsonl`, pending and unacked counts. Nothing is created.
- `sql`: `SELECT 1` and the hotel date.
- `sql.mapping`: reads `RoomTypeMapped`. An error is a `warn`, because the module creates the table on start.

### backup

- `enrollment`: the enrollment in the module data dir.
- `hub`: config and, only when something is sealed, the master key. A 401 → re-enroll remedy.
- `sql.<host>`: the password decrypts, connect works, and `BACKUP DATABASE` is permitted on each scheduled DB. Denied → `fail`; a missing DB → `warn`.
- `compression.<host>`: whether `COMPRESSION` is supported, or correctly left off on Express/Web.
- `destination.<n>`: writes a 32-byte probe object, reads it back, compares it and deletes it. This is the **only write** in the whole self-test. A failed delete is a `fail` that names the object left behind.

### arrival

- `identity`: under `install_dir`. A missing identity → `skip`.
- `tenant_key`: configured or file key opens. A missing key → `warn`; none is created.
- `vault`: a `read_dir` of the legal vault. Not created yet → `pass`.
- `cloud`: an unauthenticated GET of the poll path. It reports the negotiated protocol (`HTTP/1.1`), and `warn` on HTTP/1.0 or 5xx. No envelope is sent, so no request is taken.
- `pms`: the agent's own `tds::probe`, which reads the server version and database name.

### membership

- `config`: `config.dpapi` in `program_data_dir` opens.
- `hub`: `GET sync-config` with the agent token.
- `smile`, `smile.identity`, `smile.room_types`, `smile.folio`: `ping`, `read_identity` (the SMILE clock), `room_types` and `occupancy_columns`.
- `write`: `skip` while `read_only` is true. When writes are on, only the settings are checked: an extra-bed mode problem or `pull_jobs: false` → `warn`. No job is pulled.

## Coordinator addendum

**`legacy.services`** (Windows; `skip` elsewhere).

- It lists services through the read-only SCM.
- It matches name or display name case-insensitively with the same substrings as the hub bench script's `LEGACY_SERVICE_PATTERN`.

| Substrings | Module |
|---|---|
| `mmei`, `einvoice`, `e-invoice` | einvoice |
| `smilepmsdevice` | pms-device |
| `smilepms`, `smile pms` | pms-site |
| `smilehl` | hotellink |
| `mm.arrival`, `arrival.agent` | arrival |
| `mm.membership`, `membership.agent` | membership |
| `srbackup` | backup |
| `pdasync` | none (listed only) |

- dl-agent's own service is excluded.
- Status is `warn` when an old agent is running (or starting) and its module is enabled in dl-agent, because the work is done twice. The remedy is `sc stop` plus `sc config … start= disabled`. Otherwise `pass`, with each service listed as `name (state, module m)`.

**Configured folders.** Every module self-test builds its settings from `ctx.config()` (`modules.<name>.*`), just as the module's `run` does, and reads only the folder that resolves:

| Module | Key |
|---|---|
| einvoice | `config_dir` |
| pms-site | `config_path`, `state_dir` |
| pms-device | `appsettings_dir` |
| hotellink | `config_path` |
| arrival | `install_dir` (through `Layout::detect`) |
| membership | `program_data_dir` |
| backup | the module data dir, plus the hub's `backup_dir` |

- Legacy default paths are used only when the key is absent, which is the modules' own fallback.
- Each crate has a test that points the key at a `…/bench/<module>` folder. It asserts that the `skip` detail names that folder and that nothing was created there.
- A bench install (`%ProgramData%\dl-agent\bench\<module>`) is therefore honoured as long as the setup step writes the key into `agent.json`.

## Upload contract (checked against main's `services/agent-hub/src/domain/schemas.ts`)

| Field | Agent sends | Hub accepts |
|---|---|---|
| `run_id` | 32 hex | `^[A-Za-z0-9._:-]{1,64}$` |
| `trigger` | `cli`, `command`, `install` | `^[a-z][a-z0-9_-]{0,31}$` |
| `started_at`, `finished_at` | Unix seconds (int) | int |
| `agent_version` | `x.y.z` | `x.y.z` |
| `os` | `std::env::consts::OS` | max 50 |
| `checks[].detail` | ≤ 600 | ≤ 4000 |
| `checks[].remedy` | ≤ 300, omitted when None | ≤ 2000 |
| `checks[].id` | ≤ 100 | ≤ 100 |
| `checks[].title` | ≤ 300 | ≤ 300 |
| `checks` | ≤ 500 | ≤ 500 |
| body | ≤ 250 000 B (details cut to 200 chars if larger) | 256 KB |

- Upload path: `POST /v1/agent/selftest`. An earlier fallback to `/v1/selftest` was removed once main showed the real route.
- A 404 is logged and ignored. Any other error is reported on stderr (CLI) or in the command output (service). The run's verdict does not change: `ok = no fail`.

## Tests and gates (all on macOS x86_64, `HEAVY_LOCK=/tmp/heavy-dl-st ~/.claude/scripts/heavy.sh`)

| Command | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo test -p dl-agent -p dl-agent-einvoice -p dl-agent-pms -p dl-agent-pms-device -p dl-agent-hotellink -p dl-agent-backup -p dl-agent-arrival -p dl-agent-membership -p dl-agent-bin` | **1253 passed, 0 failed, 3 ignored** (68 test binaries), no warnings |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings` | clean |
| `cargo wasm` | clean |
| Files ≤ 800 lines | largest touched file: `dl-agent/src/agent.rs` at 763 |

New tests:

| Area | Count |
|---|---|
| `dl-agent` `selftest/tests.rs` | 18 |
| `dl-agent` hub upload | 2 |
| einvoice | 3 |
| pms-site | 1 |
| pms-device | 2 |
| hotellink | 3 |
| backup | 6 |
| arrival | 3 |
| membership | 4 |

They cover:
- not-enrolled → `skip` at a bench path, with nothing created
- sanitizer masking
- body caps
- legacy matching and the doubling verdict
- the backup round trip and a failed delete
- Express without compression
- hotellink never rewriting the config
- arrival never creating a key
- the membership write side `skip` while read-only
- no secret echoed from a bad connection string

## Verified only on macOS (not on Windows)

These Windows paths compile and pass clippy for `x86_64-pc-windows-gnu`, but never ran:
- service and recovery actions, the watchdog task, `legacy.services` (SCM listing)
- DPAPI decrypts: hotellink, membership, arrival tenant key, einvoice `config.enc`
- ACL permissions

No module check ran against a real SQL Server, hub, cloud or destination. Those paths were tested only through fakes (backup) or through unreachable and invalid inputs (the other crates).

## Concerns and open gaps

1. **A CLI self-test while the service is down sends its own heartbeat.** That heartbeat can take queued commands. They are answered with "taken by the command-line self-test …; send it again" instead of being lost, but the hub has to resend them.
2. **pms-site `cloud` mints a real short-lived token** (the module's normal auth exchange), and hotellink, membership and backup call the hub with the agent token. These calls are reads on the hub but appear in its logs.
3. **No C4 schema requirements are registered by the modules yet.** Core `sql.<name>` runs a schema probe only when the hub's `selftest` command carries `{requirement, database}`. The module checks (einvoice `missing_procs`, membership `occupancy_columns`, hotellink `RoomTypeMapped`) cover their own needs.
4. **The legacy service names are inferred.** Substrings are used because the exact Go E-Invoice and SRBackup service names are unconfirmed, the same gap as the bench report. A miss only means the service is not listed. The `smilepmsdevice` → pms-device entry is a guess; the addendum names no device agent.
5. **The membership write side, when enabled, checks settings only.** The SMILE schema probe runs on the first job, because `SchemaProbe` needs the writer's session, and the self-test must not open a write transaction.
6. **The backup destination probe is a real write.** It writes, reads and deletes one 32-byte object per destination, as the brief allows. If the delete fails, the check fails and names the object left behind.
7. **Every module self-test is bounded per probe** (15–45 s) **and per module** (120 s). A hung probe thread is left to finish in the background, and its result is dropped.
