# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/); before 1.0, minor versions may change APIs.

## [Unreleased]

### Added

- agent-hub: the guided install page shows the short `irm <url> | iex` line when the product link carries `irm=1` (Check-In), with a folded "Máy cũ? dùng lệnh này" fallback (TLS 1.2 + WebClient) for old Windows; other products keep the long line (RT-0461).
- dl-agent: pda-sync runs the cloud's `legacy.export` site command (RT-0390, ADR 0136): it accepts at once with `{"accepted":true,"runId":...}` or rejects with a specific reason (already running, SQL unreachable, maintenance gate closed, invalid payload), then on a background thread exports the on-prem store (`pda.db`, or the SQL Server tables of a build from before it) into a bundle under the data folder, uploads it as a signed dry run, with `commit` commits it and checks the count with a second dry run, deletes the bundle and zip whatever happened, and reports `LEGACY_EXPORT_STARTED` / `LEGACY_EXPORT_FAILED` (stage, table, SQL number, code; never row data) / `LEGACY_EXPORT_UPLOADED` as site events of sources `legacy-export` and `legacy-upload`.
- dl-agent: `dl-agent pms-device-enroll --api-url URL [--agent-id ID] [--appsettings-dir DIR] [--token-file PATH] [--force]` makes the `pms-device` module startable: it writes `Agent:ApiBaseUrl`, `Agent:AgentId` (default: the computer name) and `Agent:TokenFile` into the DeviceAgent `appsettings.json` (other keys kept; a file with comments or trailing commas is refused, not overwritten) and the token into a private token file, from `DL_AGENT_PMS_DEVICE_TOKEN` or generated (32 random bytes, hex). It prints `sha256=<hex>` of the token for the PMS operator to register; the token is never printed. An existing different token is replaced only with `--force` (RT-0422).
- agent-hub: remote attach and the demo bench enrol `pms-device` with `pms-device-enroll` (`DL_AGENT_DEMO_PMS_DEVICE_URL`, optional `DL_AGENT_DEMO_PMS_DEVICE_TOKEN` and `DL_AGENT_DEMO_PMS_DEVICE_AGENT_ID`); the hub passes the module's `appsettings_dir` as `--appsettings-dir` (RT-0422).

### Fixed

- dl-agent: pda-sync re-stamps queued batches with the new agent id after `pda-enroll` changes the identity, so they are no longer refused (`Batch agent_id does not match`) and quarantined, and the recovery resnapshot after an identity change skips the 6 h cooldown (Muine Bay, RT-0450).
- dl-agent: pda-sync skips orphan rows right away when the outbox is full but a few bytes under its cap, instead of after 30 minutes (RT-0442).
- agent-hub: remote attach of `pms-site` passes the module's `config_path` as `--config` and `state_dir` as `--state-dir`, so `pms-enroll` writes where the module reads instead of the default `C:\ProgramData\SmilePMS\Agent` (RT-0422).
- agent-hub: a remote attach with `reenroll: true` and a new `external_id` removes the agent's other `product_links` of that product, so the old tenant no longer lingers (RT-0422).

### Changed

- agent-hub: `GET /admin/agents` and the status page list only live machines; revoked agents show with `include_revoked=1` (also `list_agents` in the MCP tools), and `GET /admin/agents/:id` still finds them (RT-0375). Policy: real customer tenants are `demo=false` (stable), canary only for our machines and consented pilots, a machine ahead of stable is pinned to canary until stable catches up (ADR 0004, README "Release channels for customers").

### Fixed

- dl-agent: a hotel whose outbox is full no longer stays stuck behind a batch waiting for parent rows (Muine Bay, `OUTBOX_FULL`, RT-0225). Once the agent has asked for the parent stream to be resent and capture still cannot (the outbox is at its cap, or twice the wait has passed), it sends the waiting batch without its rows that have no parent, so the outbox drains and capture resumes; the stream is already queued for a full resend, so those rows come back.
- dl-agent: `pda-enroll` no longer fails where the service runs as LocalSystem and SQL Server takes Windows authentication only (Muine Bay, RT-0365). With no `--sql-admin-user` or `--sql-admin-saved` it tries the Windows identity first and, when SQL Server refuses it (`SQL_NO_ACCESS`, `SQL_LOGIN_FAILED`, `SQL_DATABASE_NOT_FOUND`), the SMILE default login `smile` to create `pda_sync` (never altered). Its password is `DL_AGENT_PDA_SQL_DEFAULT_PASSWORD` or the default built into the release; the user can be set with `DL_AGENT_PDA_SQL_DEFAULT_USER`. Each attempt is a console line and an `ENROLL_SQL_ADMIN_ATTEMPT` site event without the password, and when both fail the error lists them and suggests `--sql-admin-user`. The hub's pda-sync attach accepts the new password variable.

- agent-hub, dl-agent: remote attach of einvoice, arrival and membership enrols into the directory the agent config names for the module (`config_dir`, `install_dir`, `program_data_dir`) instead of the tool's default, so the restarted module reads the new identity (RT-0261). A module without that key is enrolled where it was before.

- dl-agent: `arrival-enroll` uses the SQL connection `dl-agent sql-setup` saved (`SMILE_FO` in `sql.json`) when none is given, saved or left by the old agent, instead of exiting 78 "no PMS connection string" (RT-0261).

- agent-hub, dl-agent: the one-line installer no longer looks like a trojan to behaviour-based antivirus (RT-0361; Kaspersky flagged `dl-agent-setup-*.exe` as `PDM:Trojan.Win32.Generic` and stopped the install). With another antivirus running (Kaspersky, ESET, Bitdefender, Bkav, ...), Windows Defender is left alone and the installer prints what to add as a Trusted application (program path, SHA-256, folders to exclude; the Kaspersky menu path). With Defender in charge, `install.ps1` adds its exclusion itself in the visible console; the binary is always called with `--no-defender`, so it never starts a hidden PowerShell to edit Defender, and on its own (`dl-agent install --hub` run by hand) it only does so when Defender is the active antivirus. The build is downloaded into `%ProgramFiles%\DevLibrary\<product>` instead of `%TEMP%`.

- dl-agent: `pda-enroll` repairs the `pda_sync` login after an earlier failed run instead of failing with 18456 for good (RT-0353). The login step is skipped only when the password the agent holds still signs in; a held password SQL Server rejects (saved by a run whose `ALTER LOGIN` then failed with 15247) gets the login reset with a new one when enroll runs as a SQL administrator. A failed login step now puts the previously held password back, so it never leaves the store out of step with SQL Server.

- agent-hub, dl-agent: remote attach covers a machine already enrolled elsewhere and a service account that is not a SQL administrator (RT-0261, follow-ups of RT-0324/0327/0328). `POST /admin/agents/:id/attach` takes `reenroll: true` (adds the tool's `--force`, moving einvoice, pms-site, arrival, membership, hotellink or backup to the new product tenant) and `flags` (value-less flags from the module's list, else `400 unknown_flag`). `pda-enroll`/`pda-grants` gain `--sql-admin-saved` (the SQL login dl-agent keeps in `sql.json` becomes the SQL administrator, its server the `--sql-instance`) and read the administrator password from `DL_AGENT_PDA_SQL_ADMIN_PASSWORD` too, so `SQL_NO_ACCESS` (error 15247 as LocalSystem) is fixed from the hub; the pda-sync row accepts `--sql-admin-user` with that secret.

- dl-agent: a probation rollback now names the module(s) that stayed unhealthy and their last error (`update_rolled_back`: `a module stayed unhealthy through probation: pms-device (…)`), so the canary gate's `no_rollback` failure can be traced without a site visit (RT-0261).

### Added

- agent-hub, dl-agent: remote attach (RT-0261). `POST /admin/agents/:id/attach {module, secret, options, external_id}` switches a product on for an agent that is already installed: the hub enables `modules.<module>`, queues one `attach` command that runs the module's enrol tool on the machine with the product's one-time code in its variable (migration `0010`: the code lives in `commands.secret`, never shown, cleared when the agent takes it or at expiry), then `restart_module` once the tool succeeds. The agent's `sys` module runs the tool as a child process (registered tools only, `DL_AGENT_*` variables only, no input, 10 minute limit, secrets masked in the answer). One install per customer; further products need no site visit.

- release: dl-agent builds, signs and publishes without the iMac (RT-0311). `scripts/release/canary.sh <version>` builds in a one-shot Fly Machine (`fly-build.sh`: Machines API over curl, `auto_destroy`, destroyed on timeout or error; `machine-build.sh` stages the unsigned binaries in R2 `dl-agent-builds/`), then signs canary and stable manifests locally, uploads and publishes canary; `rel.sh` (fetch, sign, upload, canary, gate, promote) uses repo-relative paths and reads the seed and admin credentials from the environment. `scripts/build-dl-agent.sh` gains `DL_XBUILD_NATIVE=1` (no Docker), and the toolchain moves to `scripts/xbuild/setup.sh`, shared by the Dockerfile and the build machine.

- agent-hub: ops console tool `agent_survey {agent_id}` (RT-0307): an agent's last site survey with a summary of where its SQL login came from (`login_source`) and how each database signed in (`auth_scheme`, `tds`, `server_port`, `instance`, `encryption`, `tls`), engine and warnings; read-only, facts only.

- agent-hub: the ops console's GitHub reads survive the API's rate limit (RT-0289). On a 403/429 from the unauthenticated API (shared Worker IPs), `repo_commits` falls back to github.com's commit Atom feed and `repo_pull` / `repo_commit` to the pull request's or commit's `.patch` (commits, changed files, patch; merged from main's feed), marked with `source`. Still read-only and token-free.

- agent-hub: the ops console can check a pull request on its own (RT-0287). New read-only MCP tools `repo_pull {number}` (title, state, merged, head/base sha, description, changed files with a 4000-character patch excerpt each) and `repo_commit {sha}` (message, parents, changed files) on the public repository. `scripts/publish-cargo-run.mjs [--ref <sha>] [--windows]` runs `cargo clippy -D warnings` and `cargo test --workspace` on any commit in a temporary worktree, isolated (scrubbed environment without secrets, `--offline` in a loopback-only network namespace), and publishes the counts and failures as suite `cargo`, read with `hub_test_results {suite: "cargo"}`.

- dl-agent: RT-0303 follow-up. An old agent's secret sealed with DPAPI CurrentUser (Membership `config.dpapi`, E-Invoice `config.enc`, PDA SyncAgent `pda-sync-sql-password.bin`) is recognised from the blob's flags and skipped with a warning instead of a decryption error (`legacy_sql::machine_scope`, `sealed_for_one_user`). The survey `session` also names the TLS level (`encryption` `full`/`login_only`/`none`, `tls` rung) and the named `instance`, and the self-test line shows them with the port (`sign-in NTLM, TDS 7.3B, encryption login_only, instance SMILE, port 49172`).

- dl-agent: the installer reuses the SQL login the machine already has instead of asking (RT-0303). With no `--sql-auth`/`--sql-prompt`, `install` tries, best first: the login this agent saved, the old Check-In connection the script hands over, every old agent's login the modules find (`App::sql_source`: PMS site `state\databases.json`, E-Invoice `config.enc`, Check-In `appsettings.Production.json`, Membership `config.dpapi`, HotelLink `config.json`, PDAOrder SyncAgent `agent.json` + DPAPI password; in place, `.bench-old-*` or a bench record's folder), then Windows sign-in to the local SQL Server. Each is tested on every SMILE database with `sql-setup --no-report --source …`; the first that passes is saved with `login_source`. If none passes, an interactive run shows the menu, an unattended one installs and enrols without SQL, spools one `sql_login_not_found` report (each login tried and its first error, never a password) and exits 1. `selftest` names the sign-in (`sign-in NTLM, TDS 7.3B`) and the reused source; the survey gains `session` (`auth_scheme`, `net_transport`, `server_port`, `tds`) and `login_source` (PROTOCOL §9.5).
- agent-hub: guided install page for product consoles (RT-0292). Every new enrolment code (`POST /products/:product/enroll-codes`, `POST /admin/enroll-codes`) also answers `deploy_url` (`https://agent.pms.id.vn/deploy#code=…`, the code only in the fragment). The page shows three steps (open PowerShell as Administrator, copy the line with one click, wait) and follows the machine live through `POST /deploy/status {code}`: connected host and version, each module's state and the install self-test (pass/warn/fail), then a done banner; unknown, expired and revoked codes say what to do. A product console only needs one "Deploy agent" button that opens `deploy_url`.
- agent-hub: read-only ops console for the Request Tracking assistant (RT-0300): MCP over HTTP at `/mcp` (`Authorization: Bearer MCP_TOKEN`, a Worker secret; locked without it) with `hub_test_results`, `repo_file`, `repo_list`, `repo_commits` (the public repository, default `main`), `admin_get` (any GET of the admin API, in-process, the admin token never leaves the Worker; bundle downloads and the HTML page refused) and named look-ups (`list_agents`, `get_agent`, `agent_selftest`, `list_releases`, `release_gate`, `list_reports`). `PUT/GET /admin/test-runs/:suite` keeps the last 20 `pnpm test` summaries (migration 0009); `pnpm test:publish` runs typecheck + vitest and publishes one.
- agent-hub + dl-agent: one customer, one agent (RT-0298, decision 0002). A machine enrolled for one product takes another product's one-time code with the signed `POST /v1/attach {code}`: the hub checks the code like an enrolment, deep-merges its `modules` into the agent's config (other modules and keys kept, version bumped), marks the code used and records the product link in one transaction (one winner per code; a code the agent already holds re-merges idempotently; a code without modules is refused with `400 code_has_no_modules`), with an audit row. New `product_links` table (migration 0008, backfilled from `enroll_codes.minted_by`; product-code enrolments record it too) shown as `products` on `GET /admin/agents` and the new `GET /admin/agents/:id`. The agent gains `HubClient::attach` and `dl-agent attach --code` (needs an identity), and the installer on an enrolled machine given a code attaches instead of ignoring it (`Attached <modules> to this agent`), so the same install line works for a second product; an attach failure is reported at the end after the service is back with its products.
- agent-hub: demo bench in one command (RT-0261): `POST /admin/demo-bench` mints a one-time canary enrolment code for a demo tenant with every module enabled and lists the variables the operator sets for each module code; `install.ps1 -Demo` (or env `DL_AGENT_DEMO=1`) installs canary, runs each module's enrol tool with its code in env only, restarts the service, runs `dl-agent selftest` and prints a per-module pass/warn/fail table, then clears every demo variable.
- dl-agent-pda-sync: `pda-enroll` reads the one-time code from `DL_AGENT_PDA_CODE` when `--code` is absent, so it stays out of the process list (RT-0261).
- dl-agent-hotellink: end-to-end tests against SQL Server (gate `HL_AGENT_TEST_SQL`, `src/e2e/`): a fake hub serves a booking and a mapping command over the module's long poll and the real module writes Folio, FolioRate, AdditionalName and RoomTypeMapped and acks; an ack lost at a crash is replayed byte for byte on restart with no second write; SQL down opens the PMS breaker (no apply, no SQL hammering) and it recovers. `connectionStringOverride` is read with SqlClient's rules: `TrustServerCertificate=yes/no`, `Trust Server Certificate=…`, and `Encrypt=false/no/optional` without `TrustServerCertificate` (login-only TLS, certificate not checked, like SqlClient). A stop now cuts the 25 s long poll at once (the runner's threads end in milliseconds instead of up to 25 s), and while the open breaker holds the jobs the hub hands back, the next poll waits 5 s (`Timings::breaker_poll_wait`; Go polled again at once) (RT-0259).
- dl-http: `Cancel` and `Client::request_with_cancel` end a blocking request from another thread (the socket is shut down; new `Error::Cancelled`) (RT-0259).

- dl-tds: `User ID=DOMAIN\user;Password=...` logs in as a Windows account with portable NTLMv2 (MS-NLMP, MIC when the server sends a timestamp) on every OS, also for `Integrated Security=SSPI` outside Windows when such credentials are given (SSPI stays first on Windows); new `Error::Ntlm`. Tested with the MS-NLMP 4.2.4 vectors and end to end against the fake server. Named instances (`host\INST`, `.\INST`, `(local)\INST`, URL path, explicit port precedence), SQL Server 2008 / 2008 R2 answering TDS 7.3, TLS 1.0-only engines and handshake resets are covered end to end by the fake server (`fake::TlsPeer`, TDS version, product version and scripted responses) (RT-0258).
- dl-hash: `Md4` (RFC 1320), only for NTLM (RT-0258).
- dl-agent-backup: the copy a backup sends to its destination is encrypted under the tenant's own backup data key (`GET /api/agent/backup-key`, format `DLBKENC1`: chunked AES-256-GCM, a per-file HKDF key, truncation and reordering refused) and the report names the key (`encryptionKeyId`); a restore asks the hub for the key the file names (`?id=`), so one tenant's agent cannot open another tenant's backup (clear `backup_credential_error`, no plaintext written). A hub without the endpoint (404) gets unencrypted uploads as before, and backups made before encryption restore unchanged. Live test of `RESTORE … WITH REPLACE` and restore to a new name against SQL Server 2022 with per-table row counts and `CHECKSUM_AGG(BINARY_CHECKSUM(*))` (RT-0259). The SRBackup hub side (per-tenant keys stored wrapped) is specified in `docs/plans/2026-11-19-rt0259-backup-tenant-keys.md` and not deployed.
- dl-agent: the `collect_diagnostics` bundle also carries `survey.json` and the last self-test run (`state/selftest.last.json`), and a module can name a rolling log folder (`ModuleContext::add_diagnostic_folder`, plan hook H2) whose newest files go in within a budget; tests for hub failover over real loopback HTTP and for the whole fault-probation rollback trial (RT-0258).
- dl-log: `scrub` also masks URL credentials, JWT-looking strings and fields whose name contains `password`/`secret` or has a `token`/`pwd`/`apikey` part (RT-0258).

- dl-agent-speaker: the PDAOrder speaker as the opt-in `speaker` module of dl-agent (`modules.speaker.rvcList`): joins the PDA order hub per RVC with the enrolled `pda-sync` identity (no long-lived speaker JWT file), plays the embedded alert tones and speaks vi/en through Windows SAPI with the per-RVC `guest.speakerSoundMode` rules, hub commands `status` and `test`; no print relay (pda-sync relays, ADR 0026). Needs a PDA cloud change to accept the agent for `JoinRvc` and the sound-mode route (RT-0257).
- dl-agent-membership: REDEEM jobs with the hub's `vouchers` array (voucher stacking) post one folio discount line per voucher, in `sequence` order, for exactly the amount the hub applied, with the voucher code as reference; a re-delivered job posts only the vouchers still missing, a voucher without a positive amount sends the whole job to the desk before anything is posted, and a booking with a single `voucher` behaves as before (RT-0260).
- dl-agent-membership: SMILE_SPECIAL extra beds on create and modify (agent-marked `Folio_Special` rows reconciled in the folio transaction, the .NET preflight and fold reasons, the hub's chosen special from sync-config), and with writes on the extra-bed terms and `SpecialService` catalogue reports (`api/agent/pms/extra-bed-terms`, `api/agent/pms/special-services`); new key `smile_hotel_id` (RT-0256).
- dl-agent-pda-sync: the order, combo, covers, table, check-merge, line-move, KDS and reservation commands are tested live as `pda_sync` on SQL Server 2017 and 2022 (each delivered twice: one write, one `Pda_SyncCommandLog` row, the same answer; refused after a POS WIN change); the PDA cloud link is reported to the watchdog from the first accepted heartbeat, and Error site events reach the hub as problem reports (RT-0257).
- dl-agent-pms: local op journal (request-key replay); commands `pms.status`, `pms.selftest`, `pms.op_dry_run`; first op `fo.hotelDate.get@1` (RT-0164, RT-0220).
- dl-agent-pda-sync: `sync.verify` per bucket and repair by key (ADR 0126/0128): the cloud command compares the shadow with the cloud manifest, queues differing keys in the outbox `key_repair` table, and the capture's `key-repair` job resends or tombstones them, so a row deleted while no agent ran heals after one verify; live-tested on SQL Server 2017 and 2022 (RT-0257).
- dl-agent-pda-sync: the PDAOrder SQL sync agent as the opt-in `pda-sync` module of dl-agent (SMILE_POS capture, outbox and blob upload, heartbeat, command channel with exactly-once site commands, staff sync, order grants monitor, print relay), with the `pda-enroll` and `pda-grants` tools, selftest rows and the hub commands `status`, `selftest` and `capture_dry_run`. Sites uninstall the old PdaSyncAgent and enrol fresh (docs/plans/2026-11-19-pda-sync-plan.md, RT-0164).
- dl-agent-einvoice: run-time SQL transport ladder from core (step-downs reported as `einvoice_sql_transport`); `sql_source = site` reads the machine's sql.json; hub check commands `status`, `selftest`, `collect_dry_run` and `writeback_probe` (both read-only) (RT-0164).
- dl-agent-membership: `membership-enroll --switch` takes a site over from the .NET `MM.Membership.Agent` (keeps its `config.dpapi`, or seals one from its `appsettings*.json` / portable store; a fresh code only when the hub rejects the token, keeping the SMILE connection), stops and disables its service and records the switch; `--rollback` hands the site back (RT-0256).
- dl-agent-einvoice: `einvoice-enroll --switch` replaces the Go agent by a fresh enrolment; data continuity via SQL marks and idempotent write-back (RT-0164).
- agent-hub: E-Invoice pilot script and bench helpers; install.ps1 (thin and legacy) clears `DL_AGENT_EINVOICE_TOKEN` after the setup (RT-0164).
- Installer: `irm https://agent.pms.id.vn/install.ps1 | iex` installs, repairs or updates with prompts for what is missing (hidden SQL password, env-only when unattended); the script only downloads, checks SHA-256 and the Ed25519 release signature (`dl-agent verify-self`) and hands off to `dl-agent install`. New `dl-agent update`, `dl-agent install stable|canary|auto|x.y.z` (channel choice or version pin, reported in the heartbeat), `DL_AGENT_DISABLE_AUTOUPDATE=1`, `irm .../uninstall.ps1 | iex` / `dl-agent uninstall [--purge]`; hub serves `/install/manifest/...`, `/install/legacy.ps1`, `/uninstall.ps1` (RT-0164).

- dl-agent-arrival: hub commands `status`, `vault_check`, `selftest`, `diagnostics`, `picture_probe`,
  `op_sweep` (always rolled back) and `install_procedures`, returning JSON without secrets; new selftest
  rows incl. `pms_source`; problem journal with `recentProblems` and `installations` in the hello.
- dl-agent-arrival: `arrival-enroll` keeps the legal vault readable across a fresh enrol and pre-fills the
  PMS connection with its sign-in mode; no tenant key is created at runtime; `Agent__*` env overrides.
- dl-agent-arrival: the core `sql.json` SMILE_FO entry (windows/sql auth, sealed password, pinned TLS
  rung) serves the PMS link when present, `PmsConnectionString` is the fallback
  (`arrival_core_sql_unusable` when the entry is unusable); cloud link reported to the watchdog;
  `agent-problems.json` and `dl-enroll.json` in the diagnostics bundle.
- agent-hub: the bench and the installer pre-fill SQL Server from the old Check-In agent
  (`ARRIVAL_LEGACY_SQL_BODY`).
- dl-agent: `sql-setup` picks a Windows or SQL login, tests it on every SMILE database before saving
  and seals the password with DPAPI; the installer offers the same choice (password from env
  `DL_AGENT_SQL_PASSWORD` or a hidden prompt only). The working TLS rung is saved; no silent fall back to
  an unencrypted SQL login to another host (`--allow-unencrypted` is explicit).
- dl-agent: `collect_diagnostics` uploads a scrubbed zip to the hub (R2 `diag/`, 30-day lifecycle, admin
  list and download, migration `0007`), only for an open command of that agent, once, 4 per hour.
- agent-hub: admin audit trail with the actor from a verified Cloudflare Access JWT
  (`ACCESS_TEAM_DOMAIN`, `ACCESS_AUD`); a change that cannot be recorded is refused with 503, except the
  kill switch (release pause, tenant hold), which runs and logs the audit failure.
- agent-hub, dl-agent: ordered `hub_urls` from `HUB_URLS` with failover, hosts limited to a built-in
  allowlist.
- dl-agent: proxy from env, WinHTTP or Internet Options, with Basic credentials; selftest rows
  `core/hub_urls`, `core/proxy`, `core/update_state`.
- dl-agent: modules drain before the binary swap; fault build `--fault probation` for rollback tests.
- dl-agent: module cloud links (`ModuleContext::set_cloud_link`), restarted by the watchdog when one goes
  silent (only 2xx/3xx count as alive); `add_diagnostic_file` for modules.
- dl-log: `scrub` (incl. `*_key` names, ADO.NET quoting); dl-http: `ProxyRules`; dl-tds: `fake-server`
  feature; dl-winsvc: `process::list`, `process::named`.
- agent-hub: installer hook `Get-ArrivalLegacySql`; `arrival-enroll` gets the SQL login by env.
- dl-agent-einvoice: per-tenant collect profile from the hub config (`collect_profile.pos_item_name`,
  default `till`); `iname1` takes POS line names from `ItemDef.IName1` (RT-0204 port, opt-in). Golden
  drift guard against MM-EINVOICE (`$MMEI_REPO`).
- Kill switch (PROTOCOL.md §5.3, §6.5, migration `0006_kill_switch.sql`). The hub can pause a
  release (`POST /admin/releases/:product/:version/pause`, `/resume`): it is never offered on
  canary or stable and the manifest falls back to the newest non-paused release. It can hold a
  tenant or one site (`POST/DELETE /admin/tenants/:tenant/hold`, optional `site_id`, `product`,
  `reason`): no `update_check` and no manifest, except for an explicit `update_now`. dl-agent
  honours a `<data dir>/no-update` file: no self-update, `update_blocked: "local"` in the
  heartbeat, an info report per start and a selftest warn row; `update_now` still works.
- Update lanes (docs/decisions/0001-update-lanes.md, PROTOCOL.md §5.1, §5.2, §6.4). Canary is
  the fast lane for test machines: the hub answers canary heartbeats with a 15 s interval
  (`CANARY_HEARTBEAT_SECONDS`), 5 s while an update is on probation, and dl-agent on canary
  installs on the first heartbeat after a publish (no 5-minute check spacing, no site window or
  `auto: false` hold), with a 120 s probation; the failure cap and rollback are unchanged. The
  agent now accepts heartbeat intervals down to 5 s (was 10 s). Stable releases roll out by
  semver bump, recorded on the promotion (`bump`, migration `0005_update_lanes.sql`): a patch
  soaks 2 h and reaches every stable agent at once; a minor soaks 24 h and ramps 10 → 50 →
  100 % at 0 / 24 / 48 h after promotion (`MINOR_RAMP`), by the agent's own rollout bucket and
  capped by the manifest's `rollout_percent`; a major soaks 72 h on at least 2 canary agents
  and is offered only to tenants or sites opted in with
  `POST /admin/releases/:product/:version/allow` (`DELETE` withdraws) or via `update_now`.
  `GET /admin/releases/:product/:version/gate` adds `lane` (bump, current stable, rollout
  percent, ramp, allowed tenants and sites). New hub vars `GATE_SOAK_HOURS_PATCH` (falls back
  to `GATE_SOAK_HOURS`), `GATE_SOAK_HOURS_MINOR`, `GATE_SOAK_HOURS_MAJOR`. Promotions from
  before the migration behave as a patch; 1.3.x agents keep working unchanged.
- agent-hub installers reuse the SQL Server connection dl-agent already holds: the bench link
  offers it on a rerun or reinstall (Enter keeps it; a failed or declined one prefills the prompt,
  and an empty password keeps the saved one), and `install.ps1 -SqlPrompt` keeps the declared
  databases when `sql-test` passes.
- `dl-agent-membership` (M2, write side): with `read_only: false` and `pull_jobs: true` the
  module executes and acks hub jobs against SMILE with the .NET agent's SQL: create / modify /
  cancel folios under the folio and cancel applocks (replay lookup, hand-entered duplicate
  guard, TCVN3 names, AdditionalName placeholders), member profiles under the profile applock,
  link folio (`LINK_TARGET_CHANGED`), voucher and reward discounts via `PostOneTransaction`,
  travel agent reads, all behind the DB-identity write gate. New keys `write_lock_timeout_ms`,
  `default_payment_code`, `default_breakfast_code`, `extra_bed_write_mode`. `read_only` still
  defaults to `true`. Not yet ported: earn reporting, SMILE_SPECIAL extra-bed rows, the
  checkout / walk-in scanners and the extra-bed terms sync (those jobs are left un-acked).
- `dl-agent-membership` earn (RT-0256): `EarnPoints` jobs read the stay's revenue (earn pairs
  derived from SMILE, total + breakdown, classification, PMS checkout time) and `POST
  api/agent/earn` like `JobDispatcher.HandleEarnAsync`, then ack. The checkout scan
  (`CheckoutScanner`) asks the hub for pending checkouts, reports earn for bookings whose rooms all
  checked out, walk-in earn by the first guest's phone (`earn/walk-in`), the folio-revenue audit,
  PMS-cancelled bookings (off on an older hub) and the daily checkout summary; the heartbeat now
  carries `WalkInEarnOk`. Keys `checkout_scan_enabled` (default: on when `read_only` is false),
  `checkout_interval_seconds`, `checkout_max_per_scan`, `checkout_grace_days`,
  `walk_in_earn_enabled`, `walk_in_lookback_days`, `walk_in_max_per_scan`. New tool
  `dl-agent membership-backfill-walkin --from yyyy-MM-dd` (the .NET `--backfill-from` mode).
  SMILE is only read.
- `agent-hub`: bench bundles for a test PC (RT-0164). `POST/GET /admin/bench`, `DELETE /admin/bench/:id` mint a demo-tenant enrolment code and a one-line `irm https://<hub>/bench/<token> | iex` (256-bit token, SHA-256 stored, steps sealed with AES-256-GCM under `BENCH_KEY`, limited fetches and lifetime). The PowerShell 5.1 script lists the older per-product agents without touching them, prompts once for SQL Server, applies optional `sql_fixups` to the restored copy in a transaction, runs `install.ps1 -Channel canary` (no `-Replace`), `sql-add` per database, each `dl-agent <setup>` step with its env for that process only (`{{SQL:<Db>}}`, `{{BENCH_DIR}}`), then `dl-agent selftest`, and writes a masked log under `%ProgramData%\dl-agent`.
- `dl-agent selftest [--data <dir>] [--module <name>] [--json] [--no-upload] [--trigger cli|install]` and the `selftest` remote command (sys module) run the same checks and upload the run, signed, to `POST /v1/agent/selftest` (a 404 from an older hub is logged and ignored; ≤ 500 checks, ≤ 250 KB). Core checks: version and channel, identity, data-dir permissions, Windows service and recovery actions, watchdog task, clock skew, signed heartbeat, update manifest signature (no install), report spool, every database in `sql.json` (connect, survey warnings, optional schema requirement), and `legacy.services` (the old per-product agents; `warn` when one runs next to its enabled dl-agent module). Each module registers a read-only `SelfTest` (`einvoice`, `pms-site`, `pms-device`, `hotellink`, `backup`, `arrival`, `membership`) that reads the module's configured folders (`config_dir`, `config_path`, `state_dir`, `appsettings_dir`, `install_dir`, `program_data_dir`) and reports `skip` ("not enrolled") when its config is absent. The only write is backup's probe object on each destination (written, read back, deleted). Details and remedies are sanitized (no passwords, tokens or connection strings). The CLI prints a table and exits non-zero on any `fail` (RT-0164).
- `agent-hub`: bench `remove_legacy` (default `true` for new bundles; bundles sealed before it only list, as before). Before any SQL fix-up or install, the bench script unregisters the older agents' scheduled tasks (`Unregister-ScheduledTask`, fallback `schtasks /Delete /F`), stops (≤ 30 s, then kills the PID) and `sc.exe delete`s every service matching the legacy pattern (never `dl-agent`, never a SQL Server service, even a named instance such as `MSSQL$SMILEPMS`), and renames their data/install folders to `<dir>.bench-old-<yyyyMMddHHmmss>` (never deletes). Every action is printed and in the summary, recorded in `%ProgramData%\dl-agent\bench-removed-<ts>.json` with a restore hint; the first failure stops the bench before install so the code is not spent.
- `agent-hub`: bench SQL part placeholders `{{SQL_SERVER}}`, `{{SQL_USER}}`, `{{SQL_PASSWORD}}`, `{{SQL_AUTH}}` (`sql`|`windows`) in step env (refused in args), filled from the one SQL Server prompt; values stay masked.
- `dl-agent-einvoice`: `einvoice-enroll` takes the platform token from `DL_AGENT_EINVOICE_TOKEN` when `--token` is absent (the argument wins), Windows authentication (`--sql-auth windows` / `MMEI_SQL_AUTH=windows`: no SQL login, integrated auth in the sealed DSN), and SSMS server forms (`SRV\INSTANCE`, `SRV,1433`) for `--sql-host`.
- `agent-hub`: `POST /v1/agent/selftest` (signed, ≤ 256 KB, ≤ 500 checks, newest 20 runs per agent), `GET /admin/agents/:id/selftest`, and a "Tự kiểm tra" section on the `/admin` status page with pass/warn/fail/skip counts and the failed checks. Migration `0004_bench_selftest.sql`.

- `dl-agent-backup`: Google Drive and OneDrive uploaders (OAuth refresh token, resumable and session uploads with resume after an interrupted chunk, parallel parts with manifest on OneDrive, bounded `Retry-After` handling). Only unknown providers still report `backup_destination_unsupported`.
- `dl-agent-backup`: `backup-enroll` adopts the Go SRBackup agent's identity (agent id and API key from its sealed `agent.json`, verified on `GET /api/agent/config`) instead of registering a new agent, so the hub's servers, schedules and history stay attached; a rejected key, another machine's file or another hub origin falls back to self-registration. The Go `agent.json` is never written.

### Changed
- `dl-agent pms-enroll`: the code may come from `DL_AGENT_PMS_CODE` instead of `--code`, so installers keep it out of the process list; `install.ps1` removes it after the setup step (RT-0164 / RT-0220).

- dl-agent-pms: works only on SMILE_FO, SMILE_POS and SMILE_BO; each op runs on the database its `target_db` names; settings come from core `sql.json`, `databases.json` is the fallback. All site-side MM_PMS code (MM_OpLog, migrations, migrate frames) removed; `pms-migrate` is a retired stub (RT-0220).
- dl-agent-einvoice: enrolment no longer falls back to an unencrypted SQL connection for a SQL login to another computer (core rule: Windows login or local server only); an explicit or carried-over `encrypt=disable` is kept (RT-0164).
- dl-agent-arrival: a failed SMILE_PICTURE phase names the missing grant; the install inventory lists
  processes through `dl_winsvc::process` instead of `tasklist`.

### Fixed
- dl-tds: `cargo clippy --all-targets -D warnings` is clean on macOS again: the TLS 1.0 fake-server tests (no TLS backend on Apple) no longer leave `TlsPeer` imported but unused there (RT-0258).
- dl-agent-arrival, dl-agent-membership: the Check-In cloud and the Membership hub are reached through the dl-agent core's proxy choice (environment, WinHTTP, Internet Options) instead of the environment only, as the .NET agents used the machine's proxy; Membership hub calls carry a fresh `X-Correlation-Id` like the .NET `AgentCorrelationHandler` (RT-0260).
- dl-agent-arrival: the SCM "terminated unexpectedly" event logged with an update restart (exit 75, same instant) is information, no longer a problem that turns selftest `last_problem` to warn.
- dl-agent-pms-device: the PMS device API is reached through dl-agent's proxy choice (environment, then WinHTTP, then Internet Options) like the .NET agent's system proxy, not the environment only (RT-0260).
- dl-agent-arrival: the self-test's `last_problem` warns only for a problem of the last 24 h; an older one is shown as history with `pass` (RT-0164).
- dl-agent-backup selftest: the BACKUP permission check covers only databases a schedule backs up; the other databases the hub lists on the server (MM_*, test copies) no longer fail it (RT-0164).

- dl-agent-arrival: the install report (`hello.installations`) uses the format the Check-In cloud parses (`processes=<n>`, both agents counted); 1.3.9 read as "unreadable report" in `agent_install_state`.
- dl-agent-arrival: the service ending with exit 75 (restart into a new version after an update, SCM event 7024) is imported as Information, not as the last problem.

- dl-agent: SQL error 4063 counts as no access.
- agent-hub bench: with a Check-In step and `sql_grant_service`, the service login also gets `db_owner` in SMILE_PICTURE, created first when missing; Check-In installs its picture tables from `master` and failed with SQL error 916 (RT-0164).
- einvoice: the collector install now includes the write-back procedure `sp_ApplyInvoiceWriteback` (MM-EINVOICE `writeback.sql`, missing from the port); a site without it gets it on the first cycle or on SQL error 2812 at write-back, never under a DBA-owned name (RT-0164).
- membership: connection string values in double quotes (`Server=".";Database="SMILE_FO"`, as the bench writes them) are unquoted like SqlClient does; the quoted host went to DNS (RT-0164).
- membership: a failed SMILE connection reports dl-tds's full error (OS error code included) instead of only the I/O kind; login error 4063 (no user in the database) counts as SMILE unavailable (RT-0164).

- `agent-hub`: running a bench link again on a PC whose dl-agent is already enrolled for the bench tenant keeps that agent and only runs the SQL registrations, steps, restart and self test (it used to install and enrol a second agent). New opt-in `sql_grant_service` (default `false`, test copies only): under Windows authentication the bench gives `NT AUTHORITY\SYSTEM` (the service account) a login, a user and `db_owner` in each of `sql_databases`, reporting each grant in the summary (RT-0164 test PC: every SQL module failed with `Cannot open database`).
- `dl-agent-arrival`: `Encrypt=False` in the PMS connection string now means what it means in `Microsoft.Data.SqlClient`: the login is encrypted and the certificate is not checked (`TrustServerCertificate=true` is added unless the string sets it). `arrival-enroll`'s `Encrypt=False` step-down failed on a SQL Server 2022 self-signed certificate, and its KB3135244 hint now appears only for a failed TLS handshake, not a certificate error (RT-0164 test PC).
- `dl-agent` probation no longer rolls back an update for a module that was already degraded or stopped when the update was installed (a site problem such as a missing enrolment or a revoked key); it rolled back and then reinstalled the same version every few minutes.
- `dl-agent-einvoice`: a running module reloads `config.enc` when it is re-sealed. Before,
  `einvoice-enroll --force` left it pushing with the revoked key (HTTP 401) until the service
  restarted.
- `dl-agent-einvoice`: `config.enc` and the outbox are written owner-only (0600) on Unix, where
  `config.enc` is not encrypted.
- `scripts/build-dl-agent.sh`: the Linux binary links glibc dynamically (glibc >= 2.36). The static
  build crashed (SIGSEGV in OpenSSL) when the einvoice collector and the hub heartbeat opened TLS
  at the same time: dl-tls `dlopen`s the system libssl, which brings in a second libc next to the
  static one. The script now fails if the Linux binary is static and smoke-runs it.

### Added

- `dl-agent-arrival`: the Azolo Check-In site agent (`MM.Arrival.Agent`) as an opt-in dl-agent
  module (`modules.arrival.enabled = true`; optional `install_dir`, `utc_offset_minutes`). It reads
  the .NET install in place (appsettings, DPAPI `agent-identity.bin` and `agent-tenant-key.bin`,
  `vault/`, the 48 h `agent-idempotency.jsonl`), so a site switches without enrolling again. Sealed
  poll / results link with the replay window, the `srCI_*` operation catalog (compiled in; a change
  ships with a dl-agent release) through `dl-tds` with parameters only, snapshots and deltas,
  idempotent `checkin.*` writes, the legal vault, the procedure-bundle installer and the watches,
  byte-compatible with the .NET golden outputs. `degraded` while not enrolled, when the cloud is
  unreachable for longer than one back-off, refuses the key or the clock, and while the PMS is down;
  stop gives running requests 5 s and flushes the journal. Metrics (requests ok / failed / refused,
  in flight, snapshots, last poll, PMS latency), rate-limited reports `arrival_cloud_failed`,
  `arrival_auth_failed`, `arrival_clock_skew`, `arrival_pms_failed`, `arrival_op_refused`,
  `arrival_proc_install_failed`; sub-command `dl-agent arrival-enroll` (token and PMS connection
  string from the environment or stdin). The crate is left out of `cargo wasm`.
- `dl-agent-membership` (M1, read side): the Azolo Membership agent (`MM.Membership.Agent`) as an
  opt-in dl-agent module (`modules.membership.enabled = true`). It reads the .NET installer's
  `config.dpapi`, sends the heartbeat with the SMILE identity, reads the hub's sync-config, and
  posts availability, room inventory, room types and folio observations (the folio cursor moves
  only after a 2xx). Jobs are pulled only with `pull_jobs: true` and are counted, never executed
  or acked; `read_only: false` is refused until the writers (M2) exist. New tool
  `dl-agent membership-enroll --hub-url <https origin>` (code from `DL_AGENT_MEMBERSHIP_TOKEN` or
  stdin, SMILE from `DL_AGENT_MEMBERSHIP_SMILE`; `--force` to replace). Reports
  `membership_hub_failed`, `membership_auth_failed`, `membership_smile_failed`,
  `membership_db_identity_changed`, `membership_scan_failed`. The crate is left out of
  `cargo wasm`.
- `dl-agent-backup`: the SRBackup Go agent as an opt-in dl-agent module
  (`modules.backup.enabled = true`; optional `backup_dir`, `concurrency`, `stop_grace_seconds`,
  `poll_interval_seconds`). Hub config polling, cron schedules in the schedule's time zone, a
  sealed durable queue with leases, three attempts and a dead letter, one backup per SQL Server
  instance at a time, restore requests and hub commands (once each), discovery, retention, and a
  sealed report outbox that survives hub outages. Credentials are opened with the agent master key
  (re-fetched once on a failed decrypt); a `credentialError` fails only that work, with a report.
  SQL through dl-tds (SQL login or SSPI), names and paths as parameters, `COMPRESSION` /
  `CHECKSUM` only where the edition supports them (recorded in the job's steps), a log backup on
  SIMPLE recovery fails with `backup_recovery_model`. Uploads to a local folder, S3 / R2 (SigV4,
  multipart) and Azure Blob (Shared Key or SAS, block lists); Google Drive and OneDrive report
  `backup_destination_unsupported` for now. `degraded` while not enrolled, when the hub stays
  unreachable past one back-off or rejects the agent; stop gives a running `BACKUP`
  `stop_grace_seconds`, then cancels it and keeps the job queued. Metrics (queue and outbox depth,
  jobs ok / failed / dead-lettered, bytes uploaded, last success per database), rate-limited
  reports `backup_hub_failed`, `backup_auth_failed`, `backup_sql_failed`, `backup_upload_failed`,
  `backup_credential_error`, `backup_destination_unsupported`, `backup_recovery_model`; commands
  `backup.status`, `backup.run-now`; sub-command `dl-agent backup-enroll --hub-url <origin>` (key
  from `DL_AGENT_BACKUP_KEY` or stdin). The crate is left out of `cargo wasm`.
- `dl-tds`: `CancelToken` and `Client::set_cancel` stop a running request from another thread:
  the client sends ATTENTION, the call returns `Error::Cancelled` within about 200 ms and later
  requests fail the same way until a fresh token is set.
- `dl-agent-pms-device`: the SMILE PMS device agent (`SmilePmsDeviceAgent`) as an opt-in dl-agent
  module (`modules.pms-device.enabled = true`; optional `appsettings_dir`, `overrides`). It reads the
  old service's `appsettings.json` / `appsettings.{env}.json` from its install folder and keeps
  `Agent:TokenFile` and `Agent:DataDirectory\outbox`, so results the old service left are sent.
  Claim / run / result against the PMS API with the 204 / 404 / 409 rules, `none`,
  `simulated-lock` and `smile-lock` adapters (LockSDK through the crate's only `unsafe` code, x86
  builds) and the POS transports; `degraded` without a token, with invalid settings or while the
  API keeps failing; stop gives the worker 5 s and leaves a blocked device call on its own thread
  with its result in the outbox. Metrics (claimed / ok / failed by kind, outbox pending, last
  command), rate-limited reports `pms_device_api_failed`, `pms_device_auth_failed`,
  `pms_device_adapter_failed`, `pms_device_sdk_missing`; commands `pms-device.status`,
  `pms-device.flush_outbox`. The crate is left out of `cargo wasm`.
- `dl-agent-pms`: the SMILE PMS site agent (`SmilePmsAgent`) as an opt-in dl-agent module
  (`modules.pms-site.enabled = true`; optional `config_path`, `state_dir`). It reads and writes the
  old agent's config file, `agent.key`, `identity.json` and `databases.json` where they are, so a
  live site switches without re-enrolling. Enroll, signed token, the `/agent/v1/stream` loop
  (`hello` reports `dl-agent-<version>`), allow-listed SQL ops, signed migrations and op-log
  retention; `degraded` while not enrolled or reconnecting longer than one back-off cycle; stop
  gives in-flight ops 5 s and closes with 1000. Metrics (ops in flight / ok / rejected / failed,
  last op, reconnects, migrations applied), rate-limited reports `pms_stream_failed`,
  `pms_auth_failed`, `pms_sql_failed`, `pms_migration_failed`, `pms_op_rejected`,
  `pms_config_invalid`; commands `pms.status`, `pms.reconnect`; sub-commands `dl-agent pms-enroll`
  and `dl-agent pms-migrate`. Cloud `update` frames are ignored.
- `dl-agent-einvoice`: `dl-agent einvoice-enroll` sub-command (port of `mmei-agent -enroll`: token exchange,
  prompted or scripted SQL connection, TLS fallback, sealed `config.enc`); skips when already enrolled,
  `--force` to redo. `scripts/build-dl-agent.sh` passes `MMEI_BUILD_DEFAULT_SQL_PASSWORD` (by name) to the build.
- `dl-agent`: modules add their own sub-commands (`App::tool`); words of `DL_AGENT_SETUP_ARGS` are
  appended to such a sub-command's arguments (never to built-in ones), for `install.ps1 -Setup`.
- `install.ps1`: `-Setup <sub-command>` runs a product's setup after enrolment (secrets through env
  `DL_AGENT_SETUP_ARGS`), and `-Replace <service pattern>` then removes that product's previous agent,
  keeping its files; a failure before that point leaves the old agent running.
- `agent-hub`: product keys. A product platform mints dl-agent enrolment codes for its own tenants
  (`POST /products/:product/enroll-codes`) without the admin token; the enrolled agent starts with that
  product's module enabled. Admin enrolment codes can carry an initial `config` too (migration 0003).
- `dl-agent-einvoice`: the Azolo E-Invoice agent as an opt-in dl-agent module
  (`modules.einvoice.enabled = true`). It runs the collect / push / write-back loop and the
  provider egress relay (ADR 0016: VNPT hosts only, https on 443, POST/GET, three forwarded
  headers, 60 s timeout cap, no redirects) on their own threads, reads the Go agent's sealed
  `config.enc` (`config_dir`, default the Go agent's directory) and stays `degraded`
  ("not enrolled") without it. Metrics: pending outbox, pushed total, last push time, relay
  calls / failures; reports `einvoice_push_failed`, `einvoice_sql_failed`,
  `einvoice_outbox_failed`, `einvoice_relay_refused`, `einvoice_device_replaced`, …;
  commands `einvoice.status` and `einvoice.collect_now`. Push items stay byte-identical to the
  Go agent's (golden vectors).
- `dl-agent`: opt-in modules. `Module::enabled_by_default` (default `true`); a module that
  returns `false` runs only where its remote-config section sets `"enabled": true`.
- `dl-agent-bin`: the `dl-agent` executable moved to its own package so it can register
  product modules (`cargo build -p dl-agent-bin`; `scripts/build-dl-agent.sh` updated).

- `agent-hub`: stores each agent's last site survey (malformed or oversized ones are dropped,
  never the heartbeat), per-product site requirements (Windows version, SQL Server engine,
  compat level, grants, forbidden warnings, clean schema probe), a ready / not-ready list
  per site with reasons, a `site_eligible` canary gate check, and a readiness table on `/admin`.
- `agent-hub`: Windows installer `GET /install.ps1` (PowerShell 2.0+) with public
  `/install/info` and `/install/download` routes: OS gate to the `windows7-*` build on NT < 10,
  clock check against the hub, Defender exclusion, size + SHA-256 check, removal of older
  copies, optional SQL Server prompt tested before the code is spent, re-runs keep the
  identity, Kaspersky guidance, and a wait for the first hub contact. Enrolment codes answer
  the one-line `install_command`. `release sign` takes comma-separated target/url/binary
  lists for one manifest carrying every build.
- `dl-agent`: update hardening. A version that fails 3 times in 24 h (bad download, failed
  swap or rollback) waits a day and is dropped after 9 failures; `retry_update {version?}`
  clears the count (replaces the "never again after one rollback" rule). Site policy
  `update.auto` / `update.window` holds releases unless the signed manifest says `urgent`
  (ignores the window) or `required` (ignores `auto: false`); `update_now` ignores both and
  says why nothing was installed. External watchdog: the agent writes `heartbeat.json`, and
  `install` registers `watchdog` every 5 min (Windows scheduled task as SYSTEM, systemd
  timer) that starts a stopped service, kills one stuck starting/stopping, and restarts one
  whose loop or hub link went stale while the hub answers, backing off 1 → 12 h; its reports
  reach the hub through the agent. Off-hub artifact downloads send `Cache-Control: no-cache`.
  Hub manifest schema accepts `urgent` / `required`.
- `dl-vn`: `tcvn3` module: TCVN3 (ABC) ⇄ Unicode for legacy SMILE `char`/`varchar` text.
  `decode` takes the code page the driver decoded through (cp1252 or cp1258, where
  `Vietnamese_*` collations turn TCVN3 bytes into combining marks and `Ư`/`ơ`), folds the
  uppercase base-plus-tone pairs legacy screens store, and leaves Unicode text alone;
  `decode_bytes` for `varbinary`, `encode` / `encode_for` to write TCVN3 back.
  `text::trim_padding` removes `char(n)` padding the way SQL Server compares it.
- `dl-agent`: signed SQL bundles `sql.install {bundle, dry_run?, force?}`: Ed25519-signed
  procedures, functions, views and idempotent scripts installed all-or-nothing in one
  transaction; syntax newer than the site's engine or compatibility level is refused before
  any change, `CREATE OR ALTER` is rewritten for SQL Server 2005–2014, replaced definitions
  are backed up, objects are stamped (`dl_bundle`) so re-sends only repair drift, and
  `requires` is probed before commit. `release sign-sql` signs a bundle offline;
  `install::with_repair` installs and retries once after `sql_missing_object`.
- `dl-agent`: schema probe `sql.probe {requirement}` lists missing tables, columns,
  views, procedures, functions and configuration rows per database (SQL Server 2005+
  catalogs, values bound); the last result rides the site survey. SQL session errors carry
  `sql_missing_object` / `sql_locked` / `sql_deadlock` / `sql_access_denied` /
  `sql_timeout` codes, and a refused login reports `sql_access_denied`.
- `dl-agent`: release builds for every site OS from `scripts/build-dl-agent.sh`: Windows
  10+ x64/x86 and Windows 7 / Server 2008 R2 / 2012 x64/x86, static, system-DLL-only
  import check, wine smoke. Win7 builds report the `windows7-<arch>` target.

- `dl-agent`: one on-prem agent for every project (RT-0164). Windows service and systemd
  install, enrolment and Ed25519-signed hub calls with clock-skew correction and hub
  fallback, signed self-update with probation, automatic rollback and version blacklist,
  canary and stable channels, supervised modules (panic capture, restart backoff, a
  built-in `sys` module), fingerprinted problem reports spooled on disk, remote config
  with last-known-good revert, remote commands and diagnostics. Wire contract in
  `crates/dl-agent/PROTOCOL.md`, inventory of the agents it replaces in `INVENTORY.md`.
- `dl-agent`: shared SQL Server bridge for every project on the site. Per-database pools
  declared locally in `sql.json` (0600, connection never sent to or taken from the hub),
  session reuse with ping-after-idle, discard of broken or mid-transaction sessions,
  acquire and query timeouts, a breaker with backoff for a down database, hub tuning of
  limits, a built-in `sql` module (`sql_unreachable` / `sql_recovered` reports, `sql.ping`,
  `sql.status`), `ModuleContext::sql`, and `sql-add` / `sql-test` commands. PROTOCOL.md §9.
- `services/agent-hub`: Cloudflare Worker control plane for `dl-agent` (D1, R2):
  enrolment codes, heartbeats, commands, per-product release lines, canary gate before
  promotion to stable, problem reports and a read-only status page.
- `dl-agent` SQL bridge: transport ladder for old SQL Servers (as configured → TLS 1.0 →
  unencrypted on a server-side handshake failure or a reset after it, as SQL Server 2008 R2
  RTM does; kept until restart, pinnable with `transport`, `sql_unencrypted` report, rung in
  stats and `sql-test`), `SET LOCK_TIMEOUT` on every session (`lock_timeout_seconds`,
  default 8 s); default query timeout 15 s (was 30), below the hub's 20 s wait.
- `dl-agent` site survey: the agent host (OS version, NT build, OS vs process arch,
  `legacy_windows`, Win7 build) and, per database, engine version/name/edition, compat
  level, server and database collations, code page, login, grants (sysadmin, db_owner,
  CREATE PROCEDURE/FUNCTION/TABLE, ALTER dbo, EXECUTE, VIEW DEFINITION, BACKUP) and
  warnings such as `engine_pre_2012`, `compat_below_2012`, `cannot_install_procedures`.
  Taken on first contact and daily, forced by `sql.survey`, sent in the heartbeat
  `survey` field when it changes or is a day old. The query runs on SQL Server 2005+.

### Fixed

- dl-agent-einvoice: keep `origin_folio_number` and `number_cover` on swapped POS-charge lines (port of
  MM-EINVOICE RT-0215); golden vectors refreshed.
- `dl-agent-membership` reports the released dl-agent version to the Membership hub
  (heartbeat, enroll, User-Agent) instead of the crate's own 0.2.1.
- `dl-rand`: Windows 7 builds (`*-win7-windows-*`) take entropy from `RtlGenRandom`;
  `ProcessPrng` (bcryptprimitives.dll) does not exist before Windows 8, so the agent
  could not start there.

## [0.2.1] - 2026-11-15

### Fixed

- `dl-ffi` `DL_VIETQR_FOLD_TEXT`: a word ending exactly at character 25 followed by more
  text produced a 27-character field 62 and an invalid payload ("Thanh toán hoá đơn bàn số
  12"). Folded text is now always at most 25 characters, without leading or trailing
  spaces; every prefix of a long test sentence is checked.
- .NET package: native libraries are at `runtimes/<rid>/native/<file>` (0.2.0 nested the
  folder twice). Crate versions now follow the release version.

## [0.2.0] - 2026-11-15

### Added

- `dl-ffi`: C ABI (`include/dl_ffi.h`, ABI version 1) so other languages call the Rust
  implementation instead of porting it: amount in words (every `i64`, linh/lẻ, nghìn/ngàn,
  bốn/tư, tỷ/tỉ, with or without "Âm", "chẵn"), diacritics removal, VietQR payload build
  (with optional folding of the message to the printable ASCII field 62 requires) and
  verification. Caller-owned buffers only; no allocation crosses the boundary. About
  130 KB native, 45 KB WebAssembly.
- `bindings/dotnet`: `DevLibrary` NuGet package for .NET 8 and 10 (`VnWords`, `VnText`,
  `VietQr`), source-generated `LibraryImport`, no dependencies, AOT compatible. Tested on
  Windows and Linux.
- Release workflow: native libraries for win-x64, win-arm64, linux-x64, linux-arm64,
  osx-x64 and osx-arm64, the WebAssembly module and the NuGet package are attached to
  every tagged release.

## [0.1.0] - 2026-11-15

First public release. All crates have no third-party dependencies and build for Linux,
macOS, Windows and `wasm32-unknown-unknown`.

### Added

- `dl-json`: strict RFC 8259 parser and serializer, ordered `Map`, raw exact numbers,
  RFC 6901 JSON Pointer, `json!` macro, `ToJson` / `FromJson`.
- `dl-decimal`: 38-digit decimal with scale 0–28, seven rounding modes, money helpers
  (`percent_of`, `split`, `allocate`, `round_to_multiple`), grouped formatting; tested
  against 8,641 Python `decimal` vectors.
- `dl-time`: `Date`, `Time`, `DateTime`, `Offset`, `OffsetDateTime`, `Duration`;
  RFC 3339, ISO 8601, HTTP dates; .NET-style patterns with English and Vietnamese
  locales.
- `dl-log`: structured logging macros, `RUST_LOG`-style filters, text / JSON / CLEF /
  logfmt formats, console, rolling file, memory and async sinks.
- `dl-uuid`: RFC 9562 UUIDs (v3, v4, v5, v7, v8), all text forms, .NET `Guid` byte layout
  and SQL Server ordering.
- `dl-jwt`: HS256 / HS384 / HS512 JWTs with strict validation and `kid` key selection.
- `dl-otp`: HOTP / TOTP, `otpauth://` URIs, replay protection, recovery codes.
- `dl-qr`: QR Code Model 2 encoder (versions 1–40), SVG and text output; bit-exact with
  segno on 85 vectors and decoded by zxing-cpp.
- `dl-cli`: argument parser with subcommands, environment fallbacks, suggestions and
  generated help.
- `dl-vn`: Vietnamese amount in words, diacritics, collation, tax codes, phone numbers,
  CCCD and VietQR.
- `dl-hash`, `dl-encoding`, `dl-rand`, `dl-bench`: foundation crates.

[0.2.1]: https://github.com/vzeturn/DevLibrary/releases/tag/v0.2.1
[0.2.0]: https://github.com/vzeturn/DevLibrary/releases/tag/v0.2.0
[0.1.0]: https://github.com/vzeturn/DevLibrary/releases/tag/v0.1.0
