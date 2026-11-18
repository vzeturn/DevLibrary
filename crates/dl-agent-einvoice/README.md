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

## Turning it on

The `dl-agent` executable (package `dl-agent-bin`) registers the module. It is opt-in:
it stays `disabled` until the hub's remote config enables it for the site.

```json
{"modules": {"einvoice": {
    "enabled": true,
    "config_dir": "C:\\ProgramData\\MM-EInvoice",
    "poll_interval_seconds": 30
}}}
```

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Run the module on this site. |
| `config_dir` | the Go agent's: `%ProgramData%\MM-EInvoice` (else `$XDG_CONFIG_HOME` or `~/.config`, then `mm-einvoice-agent`) | Where the sealed `config.enc` lives. |
| `poll_interval_seconds` | the sealed config's `poll_interval` (30 s) | Collect cycle interval. |

The platform URL, API key, device id and SQL connection strings come from `config.enc`
(DPAPI LocalMachine on Windows), written by the Go agent's enroll. A site that already
runs the Go agent switches over without enrolling again: stop the Go service, enable the
module. Enrolling a new site is not part of this module yet.

## Behaviour

- **No `config.enc`**: the module is `degraded` with `not enrolled: no config.enc in
  <dir>` and looks again every 30 s; it is not a failure and does not restart. An
  unreadable file is reported once (`einvoice_config_unreadable`).
- **Enrolled**: the collector (a cycle at once, then every interval) and the relay worker
  run on their own threads; the module is `running` once both are up and `degraded` while
  the last cycle failed to push, read SQL or write the outbox. Blocking calls cannot be
  interrupted, so a stop waits up to 5 s for the workers and leaves one that is still
  inside a call to finish on its own; a new start waits for it.
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
`einvoice_device_replaced` (critical); `einvoice_config_unreadable` (error).

Commands: `einvoice.status` (counters, `enrolled`, `detail` as JSON) and
`einvoice.collect_now` (runs a cycle now).

## Differences from the Go agent

| Area | Go agent | This module |
|---|---|---|
| Process | its own Windows service | a module of `dl-agent` |
| Updates | self-update from the platform | none; dl-agent updates itself, platform update hints are ignored |
| Heartbeat `agent_version` | `""` | `dl-agent-<version>` |
| Outbox | bbolt `outbox.db` | `outbox.json` in the module data dir. Items left in a Go `outbox.db` are collected again from SQL (their rows were not marked collected); write-back is idempotent. |
| Logs | slog file | dl-agent's log (target `einvoice`); the heartbeat still carries the recent lines |
| SQL | go-mssqldb | dl-tds, one connection per source from `config.enc` (not the dl-agent `sql` pools) |
| Enroll, `-sql-prompt`, bootstrap | yes | not yet (slice E2); no SQL password in the source |
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
- Live SQL Server: `MMEI_TEST_MSSQL_URL=sqlserver://sa:...@host:port` runs
  `tests/legacy_live.rs`; skipped without it.
