# dl-agent-arrival

The Azolo Check-In site agent as a [dl-agent](../dl-agent/README.md) module (`arrival`). It does
the work of the .NET `MM.Arrival.Agent` (Windows service of SmileCheckin, `src/MM.Arrival.Agent`
and `src/MM.Arrival.Sync.Contracts`) inside the shared agent:

- polls the Check-In cloud outbound only (`POST /api/agent/poll`, `/api/agent/results`), every
  message sealed in the HMAC + AES envelope of `MM.Arrival.Sync.Contracts` with the agent's
  identity key and refused outside the ±2 min replay window;
- runs the requests the cloud sends against the hotel's SMILE PMS database through the fixed
  operation catalog (`assets/catalog.json`) and its `srCI_*` procedures, parameterized only, with
  snapshots, etags and deltas, cell projection, and the `checkin.*` writes answered from the 48 h
  idempotency journal when the cloud asks again;
- keeps the legal vault (ID scans and signatures sealed with the tenant key) and the `vault.*` and
  `key.*` operations;
- installs the procedure bundle (`assets/pms-procedures.sql`, byte for byte the .NET one) when the
  PMS has a different version, and runs the watches that tell the cloud a list changed.

Only the SQL error number of a PMS failure crosses the wire. TCVN3 text is converted in SQL by
`dbo.fTCVNToUnicode` inside the procedures, as in the .NET agent; the module does not convert.

## Turning it on

The `dl-agent` executable (package `dl-agent-bin`) registers the module. It is opt-in: it stays
`disabled` until the hub's remote config enables it.

```json
{"modules": {"arrival": {
    "enabled": true,
    "install_dir": "C:\\Program Files\\MM-Arrival Agent",
    "utc_offset_minutes": 420
}}}
```

`install_dir` defaults to `%ProgramW6432%\MM-Arrival Agent` (or the one other `MM-Arrival Agent`
folder under Program Files that holds `data\agent-identity.bin`); off Windows to the module's data
directory. `utc_offset_minutes` is the hotel's offset for the `{today}` of the watches (the .NET
`HotelUtcOffsetMinutes`, default 420).

## What it reads in place

A site switches from the .NET service without enrolling again. The module reads, where Install.ps1
left them:

| File | Notes |
|---|---|
| `appsettings.json`, `appsettings.Production.json` | `Agent` section, merged like .NET configuration (no `Agent__*` environment overrides). |
| `agent-identity.bin` | DPAPI LocalMachine, entropy `MM.Arrival.Agent/identity/v1`; plain JSON (0600) off Windows. |
| `agent-tenant-key.bin` | DPAPI LocalMachine, entropy `MM.Arrival.Agent/tenant-key/v1`, or `Agent:TenantKey`. |
| `vault/` | The legal vault. |
| `agent-idempotency.jsonl` | The 48 h journal of answered writes. |
| `link-status.json`, `link-heartbeat.txt` | Written for the tray and Install.ps1, same format. |

Relative paths resolve the way the .NET service saw them (its working directory was
`%SystemRoot%\System32`): an existing file there first, then under the install directory.
Settings changes take effect at the next module start.

## Enrolling a new site

```sh
DL_AGENT_ARRIVAL_TOKEN=... DL_AGENT_ARRIVAL_PMS=... dl-agent arrival-enroll --cloud-url https://checkin.example
```

The token and the PMS connection string come from those variables or from stdin (one line each),
never from the command line, and are never printed. The tool tests the PMS connection (`--no-probe`
skips it; a TLS failure of a string without an `Encrypt` key is retried with `Encrypt=False`), writes `appsettings.Production.json` and
`data\agent-identity.bin` the way Install.ps1 does (owner-only), creates the tenant key and prints
its fingerprint. It does nothing when an identity exists unless `--force` is given.

## Behaviour

- Not enrolled, or settings that cannot be used: `degraded` with the reason, looked at again every
  60 s; an invalid `modules.arrival` section stays `degraded` until the config changes.
- `running` after the first answered poll; `degraded` when the cloud cannot be reached for longer
  than one back-off, at once when the cloud refuses the key or the envelope (`clock skew` when the
  clocks differ), and while the PMS is down.
- Stop: in-flight requests get 5 s to finish, queued ones are left for the cloud to ask again, the
  journal is flushed.
- Metrics: `requests_ok`, `requests_failed`, `requests_refused`, `in_flight`, `snapshots`,
  `last_poll`, `pms_latency_ms`.
- Reports (rate-limited; no guest data, no SQL text, no keys): `arrival_cloud_failed`,
  `arrival_auth_failed`, `arrival_clock_skew`, `arrival_pms_failed`, `arrival_op_refused`,
  `arrival_proc_install_failed`.
- No module commands; the .NET updater, self-care and diagnostics are dl-agent's job.

The catalog and the procedure bundle are compiled in: a change reaches the sites with a dl-agent
release.

## Tests

`testdata/golden.json` holds outputs of the .NET code (envelopes, identity token, catalog, link
status, bundle hashes); the golden tests compare byte for byte. `tests/fake_cloud.rs` runs the
whole link against a fake cloud on a loopback socket. `tests/sql_integration.rs` runs against a
real SQL Server when `ARRIVAL_AGENT_TEST_SQL` names one (a server connection string without a
database; it drops and creates `SMILE_FO` and `SMILE_PICTURE` from `testdata/pms-fake.sql`).
