# dl-agent-pms-device

The SMILE PMS device agent as a [dl-agent](../dl-agent/README.md) module (`pms-device`). It does
the work of `SmilePms.DeviceAgent` (Windows service `SmilePmsDeviceAgent`; .NET
`apps/device-agent/src`, ported to Rust in `apps/device-agent/rust`) inside the shared agent:

- claims device commands from the PMS API (`POST /device-agent/commands/claim`, header
  `Authorization: DeviceAgent <token>`), runs them and posts the result
  (`POST /device-agent/commands/{id}/result`), with the 204 / 404 / 409 answers, the
  back-off (1 → 30 s) and the per-command timeout of the .NET agent;
- door locks through `Agent:Adapter`: `none` (DEVICE_NOT_CONNECTED), `simulated-lock`
  (development) and `smile-lock` (routes by lock type: MFCARD through `LockSDK.dll` on the
  32-bit build, DEVICE_SDK_NOT_INSTALLED on x64 and for the vendors without a DLL);
- POS hardware by `target.transport`: `tcp` (raw port 9100), `serial`, `spooler` (RAW
  Windows spooler), card reads over `serial` or `pcsc` (ACR122-class NFC), `sapi` speech,
  `opos` (the registered UnifiedPOS controls for the cash drawer and line display) and the
  `bluetooth` refusal. Off Windows the hardware verbs refuse as an absent device;
- writes every result to the outbox before sending it, so a result the API does not take
  (or one finished after a stop) is sent at the next poll or the next start.

`tests/contract/*.json` is a copy of SMILE-PMS `apps/device-agent/contract/`.
`tests/contract_drift.rs` fails when it differs from the folder named by
`PMS_DEVICE_CONTRACT_DIR` (skipped when that is unset).

The vendor DLL binding (`src/native`) is the crate's only `unsafe` code; every block carries a
`// SAFETY:` comment. The crate is native-only and is left out of `cargo wasm`.

## Turning it on

The `dl-agent` executable (package `dl-agent-bin`) registers the module. It is opt-in: it stays
`disabled` until the hub's remote config enables it for the workstation.

```json
{"modules": {"pms-device": {
    "enabled": true,
    "appsettings_dir": "C:\\Program Files\\SmilePms\\DeviceAgent",
    "overrides": {"PollInterval": "00:00:02"}
}}}
```

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Run the module on this machine. |
| `appsettings_dir` | Windows: `%ProgramW6432%` (else `%ProgramFiles%`) `\SmilePms\DeviceAgent`, the old install folder; elsewhere the module's data directory (`<data>/modules/pms-device`) | Where `appsettings.json` and `appsettings.{environment}.json` are read. |
| `overrides` | none | Keys merged over the `Agent` section, after the files and the environment (e.g. `{"Adapter": "smile-lock", "Lock": {"LockKeyType": "MFCARD"}}`). |

The settings are read with the .NET rules of the old service: `appsettings.json`, then
`appsettings.{DOTNET_ENVIRONMENT}.json`, then the `Agent__*` environment variables of the
dl-agent process, then `overrides`. The `Agent` section keeps its meaning: `ApiBaseUrl` (https,
or http on loopback), `AgentId`, `TokenFile`, `DataDirectory` (default
`%ProgramData%\SmilePms\DeviceAgent`, results in its `outbox\`), `Adapter`, `PollInterval`,
`CommandTimeout` and the `Lock` section. `RunOnce` is ignored: the module always polls.

## Behaviour

- **Waiting.** While the settings do not validate (`config invalid: …`, the validator's
  messages) or the token file cannot be read (`token not readable: …`, naming the path) the
  module is `degraded` with that reason, answers commands and looks again every 30 s. It
  never puts the token in a status, a log line or a report.
- **Running.** The worker runs on its own thread. The module turns `running` after the first
  claim round trip (a 204 counts) and `degraded` (`API failing for N s: …`) once the API has
  failed for 30 s, or at once on 401 / 403. It restarts the worker with fresh settings when
  an appsettings file or the token file changes (checked every 30 s).
- **Stop.** The worker gets 5 s. A device call that ignores the cancellation is left to finish
  on its own thread; its result goes to the outbox first and is sent at the next start.

Metrics: `claimed`, `ok`, `failed` (each an object of counts by command kind, unknown kinds
under `other`), `last_command_unix`, `outbox_pending`.

Reports (each code at most once per 10 minutes, with a `held_back` count): `pms_device_api_failed`
and `pms_device_auth_failed` (the status or the transport error, no response body),
`pms_device_adapter_failed` (the kind and the exception type, no device text),
`pms_device_sdk_missing` (a vendor SDK is not installed). Contexts carry the kind, the error code
and the command id only: no payload, card data or token.

Commands:

| Command | Answer |
|---|---|
| `pms-device.status` | the counters, `outbox_pending` and the status detail as JSON |
| `pms-device.flush_outbox` | sends the outbox now; `{"sent": n, "pending": m}` (and `error` when a send failed) |

There is no `pms-device-test` sub-command: the port has no diagnostic tool to carry over.

## Differences from the .NET agent

| Area | `SmilePmsDeviceAgent` | This module |
|---|---|---|
| Process | its own Windows service and .NET host | a module of `dl-agent` (no host lifetime lines) |
| Logs | console, Warning and above to the Event Log | dl-agent's log, target `pms-device`, the same messages with `category` and `exception` fields; exceptions without stack traces |
| Settings | appsettings files and the service's environment | the same files; the environment is dl-agent's, so set what the old service had in its environment through `overrides` |
| `RunOnce` | one poll, then exit | ignored |
| Proxy | `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` and the Windows proxy | dl-agent's choice: the variables, then WinHTTP, then Internet Options |
| Result bodies | chunked | `Content-Length`, same bytes |
| OPOS | refuses every verb | drives the registered CashDrawer and LineDisplay controls, else the same refusal |

Like the .NET agent, local log lines can contain what a device or the API said; dl-agent does
not ship its log anywhere. Reports never carry it.

## Switching a workstation over

1. Install or update `dl-agent` on the workstation. Where an MFCARD encoder is used, install
   the 32-bit build (`windows-x86`, or `windows7-x86` on Windows 7): `LockSDK.dll` is x86 only.
2. Stop and remove the old service: `sc stop SmilePmsDeviceAgent`, `sc delete SmilePmsDeviceAgent`.
   Keep `C:\Program Files\SmilePms\DeviceAgent` (the appsettings files) and
   `%ProgramData%\SmilePms\DeviceAgent` (the token file and the outbox).
3. Enrol it when the PC has no DeviceAgent appsettings yet: `dl-agent pms-device-enroll --api-url
   https://<pms api>` (optional `--agent-id`, `--appsettings-dir`, `--token-file`, `--force`; token
   from `DL_AGENT_PMS_DEVICE_TOKEN` or generated). It writes `Agent:ApiBaseUrl`, `Agent:AgentId` and
   `Agent:TokenFile`, and prints `sha256=<hex>` of the token: register `{"agentId","workstation","sha256"}`
   in the PMS (`DEVICE_AGENT_TOKENS`). The token is never printed.
   Enable the module: `modules.pms-device.enabled = true`, with `appsettings_dir` only when the
   old agent was installed elsewhere and `overrides` for what its service environment set.
4. Check `pms-device.status`: the detail reads `polling <url> as <agent id> (<adapter>)` and
   `outbox_pending` is 0 once the old results are sent. Do not run both: two agents with one
   token claim from the same queue.

## Tests

```text
cargo test -p dl-agent-pms-device
```

- Unit tests per source file (ported, plus settings, prepare, telemetry and the module timings).
- The port's `tests/*.rs` (the C# xUnit cases, lock and POS contracts, transports, OPOS).
- `fake_api`: the module against a fake PMS API on a local socket (claim → `simulated-lock` →
  result, 404 / 409 dropped, a result never taken kept in the outbox, API and auth failures,
  a print job to a raw-TCP printer).
- `module_lifecycle`: opt-in, no token, invalid settings, a stop with a blocked device call
  whose result is sent at the next start.
- `contract_drift`: set `PMS_DEVICE_CONTRACT_DIR=<SMILE-PMS>/apps/device-agent/contract`.

The port's `e2e/run.ps1` (side by side with the C# agent) is not carried over.
