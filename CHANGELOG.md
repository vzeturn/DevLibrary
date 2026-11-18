# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/); before 1.0, minor versions may change APIs.

## [Unreleased]

### Fixed

- `dl-agent-einvoice`: `config.enc` and the outbox are written owner-only (0600) on Unix, where
  `config.enc` is not encrypted.
- `scripts/build-dl-agent.sh`: the Linux binary links glibc dynamically (glibc >= 2.36). The static
  build crashed (SIGSEGV in OpenSSL) when the einvoice collector and the hub heartbeat opened TLS
  at the same time: dl-tls `dlopen`s the system libssl, which brings in a second libc next to the
  static one. The script now fails if the Linux binary is static and smoke-runs it.

### Added

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
