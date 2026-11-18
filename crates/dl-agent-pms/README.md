# dl-agent-pms

The SMILE PMS site agent as a [dl-agent](../dl-agent/README.md) module (`pms-site`). It does
the work of `pms-agent` (Windows service `SmilePmsAgent`; Go `apps/site-agent`, ported to Rust
in `apps/site-agent/rust`) inside the shared agent:

- enrolls with a one-time code, keeps the site key (`agent.key`, DPAPI machine scope on
  Windows, 0600 elsewhere) and `identity.json`, and exchanges a signed request for a stream
  token;
- holds the WebSocket stream to the PMS cloud (`/agent/v1/stream`: `hello`, ping/pong, ops,
  at most 8 in flight, close codes, back-off 1 → 30 s);
- runs allow-listed ops against the hotel's SQL Server (`MM_PMS`, `SMILE_FO`, `SMILE_BO`,
  `SMILE_POS` from `databases.json`): procedures from the embedded ops manifest, binds,
  request-key replay (`MM_Op_GetLog` / `MM_Op_CreateLog`), deadlock retry, result caps;
- applies signed migration bundles (backup gate, drift check, dry run) and purges `MM_OpLog`
  on a schedule.

`assets/ops-manifest.json` is a copy of SMILE-PMS `apps/site-agent/internal/ops/ops-manifest.json`.
`tests/ops_manifest_drift.rs` fails when it differs from the file named by `PMS_OPS_MANIFEST`.

## Turning it on

The `dl-agent` executable (package `dl-agent-bin`) registers the module. It is opt-in: it stays
`disabled` until the hub's remote config enables it for the site.

```json
{"modules": {"pms-site": {
    "enabled": true,
    "config_path": "C:\\ProgramData\\SmilePMS\\Agent\\config.json",
    "state_dir": "C:\\ProgramData\\SmilePMS\\Agent\\state"
}}}
```

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Run the module on this site. |
| `config_path` | `$PMS_AGENT_CONFIG`, else `SmilePmsAgent`'s: `%ProgramData%\SmilePMS\Agent\config.json` (off Windows the relative `pms-agent.json`, so set it there) | The PMS agent config file (`cloud_url`, `state_dir`, `enrollment_code`, `log_level`, …). |
| `state_dir` | the config file's `state_dir`, else `SmilePmsAgent`'s (`…\Agent\state`) | Where `agent.key`, `identity.json` and `databases.json` live. |

The config file and the state directory are read and written exactly as `SmilePmsAgent` does,
including its `PMS_AGENT_*` environment overrides. The module adds nothing under its own data
directory (`<data>/modules/pms-site`).

## Enrolling a new site

`dl-agent pms-enroll --cloud-url URL --code CODE [--config PATH] [--state-dir DIR] [--force]`
writes `cloud_url` and `enrollment_code` into the config file (created 0600 / with the state
ACL, other keys kept, written atomically). The module enrolls with the code within 30 s, or at
once on `pms.reconnect`, and removes the code from the file afterwards.

- **Already enrolled** (`identity.json` in the state dir): prints one line and changes nothing.
  `--force` removes `identity.json` so the next attempt enrolls with the new code.
- The code is never printed or logged.
- No service step: dl-agent installs its own service.

## Migrating by hand

`dl-agent pms-migrate --bundle DIR [--dry-run] [--check-drift] [--rerun-partial N] [--config PATH]`
is `pms-agent migrate`: it verifies the signed bundle against the pinned keyring, then checks
drift, dry-runs or applies it on `MM_PMS`, and prints the result as JSON. Flags take two dashes
(dl-cli), not the Go agent's one. A failure exits with dl-agent's config exit code.

## Behaviour

- **Not enrolled** (no `identity.json`, no code): `degraded` with `not enrolled: …`, looks again
  every 30 s; not a failure, no restart, no report.
- **Config invalid** (unreadable file, bad `cloud_url`, …): `degraded` with `config invalid: …`
  and one `pms_config_invalid` report; values from the file are not repeated.
- **Enrolled**: the agent runs on a worker thread. The module is `running` once the cloud's
  first frame arrives, and `degraded` with the reason once the stream has been reconnecting for
  longer than one back-off cycle (30 s).
- **Terminal stop from the cloud** (4409 superseded, 4426 contract, the token exchange refusing
  a revoked agent, a refused enrollment code): the agent stops, the module stays `degraded` with the next step and files a
  critical report; it starts again when the config file or `identity.json` changes, or on
  `pms.reconnect`.
- **Stop**: in-flight ops get up to 5 s, then are cancelled; the stream is closed with 1000.
- `update` frames from the cloud are logged and ignored: dl-agent updates itself.

Metrics: `connected`, `ops_inflight`, `ops_ok`, `ops_rejected`, `ops_failed`, `last_op_unix`,
`reconnects`, `migrations_applied`.

Problem reports (each code at most once per 10 minutes, the next one counts what was held back;
op names, error codes, SQL error numbers and ids only, never SQL text or bind values):
`pms_stream_failed`, `pms_auth_failed`, `pms_sql_failed`, `pms_migration_failed`,
`pms_op_rejected`, `pms_config_invalid`.

Commands: `pms.status` (the counters, `connected` and `detail` as JSON) and `pms.reconnect`
(closes the stream with 1000 and connects again; also wakes a waiting or parked module).

## Differences from the Go agent

| Area | `SmilePmsAgent` | This module |
|---|---|---|
| Process | its own Windows service, `install` / `uninstall` | a module of `dl-agent` |
| Updates | self-update from cloud `update` frames | none; dl-agent updates itself, `update` frames are ignored |
| `hello` `agent_version` | the agent's version | `dl-agent-<version>` (at most 32 characters) |
| Logs | rotated service log file | dl-agent's log (target `pms-site`, same event names) |
| Terminal stop | the service exits 0 | the module parks `degraded` with the next step |
| CLI flags | `-bundle`, `-dry-run` | `--bundle`, `--dry-run` (`dl-agent pms-migrate`) |
| SQL pools | own pools over `databases.json` | the same (not dl-agent's `sql` module) |

## Switching a live site over

1. Install or update `dl-agent` on the machine (the hub enrolment is separate from PMS's).
2. Stop and uninstall the old service: `pms-agent uninstall` (or `sc stop SmilePmsAgent`,
   `sc delete SmilePmsAgent`). Keep `%ProgramData%\SmilePMS\Agent`: it holds the identity.
3. Enable the module for the site: `modules.pms-site.enabled = true` (paths only when the old
   agent used a non-default config).
4. Check `pms.status` (`connected: true`). If the old service still runs, the cloud closes one
   stream with 4409; the module parks with "stop or uninstall the other agent", then send
   `pms.reconnect`.

No re-enroll is needed: the module uses the same `agent.key`, `identity.json` and
`databases.json`.

## Tests

```text
cargo test -p dl-agent-pms
```

- Unit tests per source file (ported, plus telemetry, settings, prepare and the tools).
- `wire_fixtures`, `ops_contract`, `bundle_golden`: the port's golden frames, op fixtures and
  signed bundle.
- `fake_cloud`: the module against a fake PMS cloud on a local socket (enroll, token, hello, ops,
  reconnect after a close, refused code, superseded close, `pms.reconnect`).
- `module_lifecycle`: opt-in, not enrolled, invalid config, prompt stop with an op in flight, a
  hung op cancelled after the grace.
- `sql_integration`: set `PMS_AGENT_TEST_SQL=sqlserver://sa:<pw>@127.0.0.1:14333?encrypt=disable`
  (a SQL Server 2022 container works); skipped without it.
- `ops_manifest_drift`: set `PMS_OPS_MANIFEST` to SMILE-PMS's manifest; skipped without it.
