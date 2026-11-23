# dl-agent-einvoice

Azolo E-Invoice as a [dl-agent](../dl-agent/README.md) module (`einvoice`). It does the
work of the MM-EINVOICE on-prem agent (Go `cmd/agent`, ported to Rust in
`MM-EINVOICE/rust/agent`) inside the shared agent:

- reads pending bills from the hotel's SMILE_FO / SMILE_POS SQL Server databases (VAT
  derivation and apportionment, void pairs, POS-charge enrichment, TCVN3 text);
- pushes them to the E-Invoice platform through a durable outbox, polls write-back
  commands and writes the issued invoice numbers back;
- relays the platform's VNPT portal calls from the hotel's Vietnamese address
  (MM-EINVOICE ADR 0016).

Push items are byte-identical to the Go agent's: `tests/golden.rs` replays 800 FO / POS
cases generated from the Go code, `tests/legacy_golden.rs` the SQL Server 2008 script
rewrite and DSN text.

## Collect profile

Customers differ, so customer-specific collection is an opt-in per-tenant option the hub
sets in `GET /config`; the shared procs (`sp_FetchFO`, `sp_FetchPOS`, `MMEI_SourceHash`)
are never changed for it. A missing key, missing field or unknown value means the old
behaviour (unknown values are logged once at warn).

```json
{"collect_profile": {"pos_item_name": "till"}}
```

| Key | Values | Meaning |
|---|---|---|
| `pos_item_name` | `till` (default), `iname1` | `iname1` replaces each POS revenue line name (TrnCode 100) with `ItemDef.IName1` when filled in, looked up per check after the POS fetch (also for POS checks pulled into an FO bill). Blank or missing keeps the till name; a site without the column keeps till names. The name is not part of the source hash. `till`: no extra query. |

### Pending selection (RT-0784)

`sql/collect/sp_SelectPending{FO,POS}.sql` take an optional `@Mode`. The module probes
`sys.parameters` once per source and, when the SP has it, asks for never-sent bills every
cycle (mode 1, no hashing), re-hashes bills dated in the last 48 h every 2 minutes (mode 2)
and pages through every sent bill once a Vietnam night from 02:00 (mode 3, 500 per cycle).
A site whose SP predates RT-0784 keeps the single combined call (mode 0); the SP only
changes at enrolment. An edit-detection failure keeps the never-sent keys and is reported
as a SQL failure of the cycle. Port of MM-EINVOICE `internal/agentcollector/collectsource_pending.go`.

`tests/golden_drift.rs` fails when `testdata/golden.json` differs from MM-EINVOICE's copy
and `$MMEI_REPO` points at that checkout (skipped otherwise).

## Turning it on

The `dl-agent` executable (package `dl-agent-bin`) registers the module. It is opt-in:
it stays `disabled` until the hub's remote config enables it for the site.

```json
{"modules": {"einvoice": {
    "enabled": true,
    "config_dir": "C:\\ProgramData\\MM-EInvoice",
    "poll_interval_seconds": 30,
    "sql_source": "sealed"
}}}
```

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Run the module on this site. |
| `config_dir` | the Go agent's: `%ProgramData%\MM-EInvoice` (else `$XDG_CONFIG_HOME` or `~/.config`, then `mm-einvoice-agent`) | Where the sealed `config.enc` lives. |
| `poll_interval_seconds` | the sealed config's `poll_interval` (30 s) | Collect cycle interval. |
| `sql_source` | `sealed` | `sealed`: the SQL connection strings sealed in `config.enc`. `site`: the databases declared in dl-agent's `sql.json` (`dl-agent sql-setup` / `sql-add`), which also allows Windows authentication as the service identity. A `site` source that does not resolve keeps the sealed one and files `einvoice_sql_source`. |
| `fo_database` / `pos_database` | `SMILE_FO` / `SMILE_POS` | With `sql_source: site`: the `sql.json` database names. `pos_database: ""` = the FO database holds POS bills too. |

The platform URL, API key, device id and SQL connection strings come from `config.enc`
(DPAPI LocalMachine on Windows), sealed by `einvoice-enroll`. A site that runs the
MM-EInvoice Go agent is switched by uninstalling that agent and enrolling afresh
(`einvoice-enroll --switch`, below): a new device and key, only the SQL connection, collect
window and the database marks carry over.

## Enrolling a new site

`dl-agent einvoice-enroll --token T --platform-url URL [--sql-host H] [--sql-user U]
[--sql-password P | --sql-prompt] [--collect-from YYYY-MM-DD] [--config-dir DIR] [--force]
[--switch]` (the token may come from `DL_AGENT_EINVOICE_TOKEN` instead of the command line)
is the port of `mmei-agent -enroll` (`src/enroll/`): it exchanges the one-time token, tests the
FO and POS databases (falling back to TLS 1.0, then no encryption, for old SQL Servers),
installs the collection procedures and seals `config.enc`. `install.ps1 -Setup einvoice-enroll`
runs it after the dl-agent enrolment, with the arguments from `DL_AGENT_SETUP_ARGS`.

- **Already enrolled**: when `config.enc` in the config dir opens, it prints one line and exits
  0 without spending the token; a present but unreadable one is an error and is never
  overwritten. `--force` enrolls again.
- **Switch from the Go agent** (`--switch`): the Go agent's `config.enc` has the same folder and
  format, so the tool records the device id it sealed in `<data>/modules/einvoice/enrolled.json`.
  With `--switch` a `config.enc` with that device id is kept (the token is not spent), any other
  one is enrolled over: SQL host, login, password and collect window are read from it (or from a
  `MM-EInvoice.bench-old-*` folder the SR bench moved aside) when the arguments give none. The
  installer passes it on every run (`install.ps1 -Setup einvoice-enroll -Replace '*MM-EInvoice*','MMEI-Agent'`),
  which removes the Go service only after the setup succeeded. The exchange revokes the Go
  agent's key: if a setup fails after it, run the installer again with a fresh token.
- **Data continuity**: the Go agent marks a bill `MMEI_Collected` only after a successful push, so
  what was still in its `outbox.db` is collected again and the platform deduplicates it on
  (tenant, source_ref) and the payload hash; write-back is idempotent (`tests/runtime_live.rs`).
- `--config-dir` defaults to the module's own default; pass it when the remote config sets a
  custom `config_dir`.
- **SQL password**: `--sql-password`, `MMEI_SQL_PASSWORD`, then the build default compiled in from
  `MMEI_BUILD_DEFAULT_SQL_PASSWORD` (set by the release build, never in source). Without any of
  them and without `--sql-prompt` it refuses before the exchange.
- The exchange reports `dl-agent-<version>` in the `X-Agent-Version` header; no secret appears in
  any output.
- No service step: dl-agent installs its own service.

## Behaviour

- **No `config.enc`**: the module is `degraded` with `not enrolled: no config.enc in
  <dir>` and looks again every 30 s; it is not a failure and does not restart. An
  unreadable file is reported once (`einvoice_config_unreadable`).
- **Re-sealed `config.enc`**: a running module looks at the file every 5 s. When it changes
  (`einvoice-enroll --force` seals a new API key and the platform revokes the old one) and
  has held still for one look, the workers stop and the module loads the new file. A
  replaced device recovers this way without a service restart; a removed file falls back to
  `not enrolled`.
- **SQL at run time**: with `sql_source: sealed` the dl-agent transport ladder (as sealed →
  TLS 1.0 → unencrypted, the last only for Windows authentication or a SQL Server on this
  computer) runs at run time too, starting from the rung sealed at enrolment and never above it.
  A rung that works is reused; three handshake failures in a row on it walk the ladder again.
  Each step down files `einvoice_sql_transport` (warning) once, with the rung and the reason.
  A dropped session fails that call and reconnects on the next. Enrolment keeps the Go
  agent's own fallback (TLS 1.0, then `encrypt=disable` for any host), so an old server the
  Go agent reached in clear is sealed `encrypt=disable` and stays on that one rung.
- **Enrolled**: the collector (a cycle at once, then every interval), the write-back
  worker and the relay worker run on their own threads; the module is `running` once they
  are up and `degraded` while
  the last cycle failed to push, read SQL or write the outbox. Blocking calls cannot be
  interrupted, so a stop waits up to 5 s for the workers and leaves one that is still
  inside a call to finish on its own; a new start waits for it.
- **Write-back on arrival** (MMEI RT-0785): the write-back worker long-polls the hub
  (`POST /poll?wait=10`) and applies each batch as soon as it arrives, so an issued invoice
  number reaches SMILE about a second after issuing. It shares the collector's runner and
  locks it only to apply. Its counts are reported in the next cycle's log line (with
  `select_ms`, `push_ms`, `writeback_ms`), heartbeat and problems. A hub that answers
  without `X-MMEI-Long-Poll` (built before the change) is polled once per interval.
- **Replaced device** (the platform gave the key to another machine): the workers stop,
  the module stays `degraded` and files `einvoice_device_replaced` (critical).
- **State**: the outbox is `<data>/modules/einvoice/outbox.json`.
- **Relay**: VNPT hosts only (`*.vnpt-invoice.com.vn`), https on 443, no user info,
  `POST` / `GET`, only `SOAPAction`, `Content-Type` and `Accept` forwarded, timeout capped
  at 60 s, redirects returned to the platform unfollowed, answers over 8 MiB refused. A
  platform without the relay endpoint is asked again every 5 minutes; poll failures back
  off 5 s doubling to 60 s. Logs carry the call id, host, status and duration, never
  bodies.

Metrics: `outbox_pending`, `pushed_total`, `last_push_at`, `relay_calls`,
`relay_failures`, plus `cycles`, `read_total`, `errored_total`, `last_cycle_at`,
`relay_refused`, `relay_poll_failures`.

Problem reports: `einvoice_push_failed`, `einvoice_sql_failed`, `einvoice_outbox_failed`
(error); `einvoice_collect_failed`, `einvoice_writeback_failed`,
`einvoice_invoices_rejected`, `einvoice_config_stale`, `einvoice_relay_refused`,
`einvoice_relay_call_failed`, `einvoice_relay_poll_failed`, `einvoice_relay_result_failed`
(warning);
`einvoice_device_replaced` (critical); `einvoice_config_unreadable`, `einvoice_sql_source`
(error); `einvoice_sql_transport` (warning).

Commands (`POST /admin/commands`, `module: "einvoice"`):

| kind | does | writes |
|---|---|---|
| `einvoice.status` | counters, health, `enrolled`, how SQL is reached (auth, login, rung), the effective collect settings | nothing |
| `einvoice.collect_now` | queues one collect cycle | what a cycle writes |
| `einvoice.selftest` | SQL auth mode and rung, procedures and their version, collect profile, last collect, outbox depth | nothing |
| `einvoice.collect_dry_run` | `{from?, to?, sources?, limit?}`: fetches and builds the bills of a window, says which would be pushed | nothing |
| `einvoice.writeback_probe` | `{source?, bill?}`: the write-back table and procedure, the login's rights, one bill's mark | nothing |

## Differences from the Go agent

| Area | Go agent | This module |
|---|---|---|
| Process | its own Windows service | a module of `dl-agent` |
| Updates | self-update from the platform | none; dl-agent updates itself, platform update hints are ignored |
| Heartbeat `agent_version` | `""` | `dl-agent-<version>` |
| Outbox | bbolt `outbox.db` | `outbox.json` in the module data dir. Items left in a Go `outbox.db` are collected again from SQL (their rows were not marked collected); write-back is idempotent. |
| Logs | slog file | dl-agent's log (target `einvoice`); the heartbeat still carries the recent lines |
| SQL | go-mssqldb | dl-tds, one connection per source from `config.enc` with the transport ladder at run time, or the dl-agent `sql` pools (`sql_source: site`) |
| Enroll, `-sql-prompt`, bootstrap | yes | `einvoice-enroll` (`--sql-prompt`, `--switch`), run by `install.ps1 -Setup`; no SQL password in the source |
| `config.enc` off Windows | AES with a hostname key | framed JSON (development only) |

TCVN3 and money rounding stay the Go agent's code on purpose, so push items cannot drift
when a site switches. `dl_vn::tcvn3::decode(_, Cp1252)` plus the agent's NBSP-to-space and
trailing-space trim gives the same text on every golden case (800 cases, 7,686 non-ASCII
strings), so moving to `dl-vn` later is a refactor the golden test can guard.

## Tests

```text
cargo test -p dl-agent-einvoice
```

- Unit tests per module; the relay worker against hand-written fakes and the real
  platform client against a fake hub on a local socket.
- `tests/module_lifecycle.rs`: the module under the real supervisor (opt-in, not
  enrolled, unreadable config, a cycle against a fake platform, commands, prompt stop).
- Golden vectors (regenerate with the Go exporters, see `MM-EINVOICE/rust/agent/README.md`).
- Live SQL Server: `MMEI_TEST_MSSQL_URL=sqlserver://sa:...@host:port?encrypt=disable` runs
  `tests/legacy_live.rs`, `tests/pos_names_live.rs` and `tests/runtime_live.rs` (dry run and
  probe write nothing, write-back is idempotent, a dropped session reconnects); skipped
  without it. Each test creates and drops its own database.
