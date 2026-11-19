# dl-agent-speaker

PDAOrder speaker module for `dl-agent` (module name `speaker`, opt-in). A port of the Go
`speaker-agent` (`apps/speaker-agent` in SML-PDAOrder, and its Rust port
`apps/speaker-agent/rust`) onto the dl-agent runtime: when a guest orders or calls staff
from a table, the machine plays the RVC's alert tone and says the phrase
("Bàn 12 đặt món", "Table 7 requests the bill", ...).

```text
{"modules": {"speaker": {"enabled": true, "rvcList": [1, 2], "soundRefreshSeconds": 120}}}
```

- **Hub link** (`hub`): one connection to the PDA cloud's `/hubs/order` over the workspace
  `dl-signalr` client (negotiate, WebSocket, JSON protocol). `JoinRvc(rvc)` for every RVC of
  `rvcList`, each waiting for its completion so a refusal is seen per RVC. Reconnects on the
  web client's schedule (0 s, 2 s, 5 s, 10 s, then 30 s by outage length), with a fresh
  token each time; when the cloud refuses every RVC it tries again after 5 minutes.
- **Events** (`decision`): `GuestOrderRequested(rvc, table, id)` is an `order`;
  `GuestServiceRequested(rvc, table, id, type)` is `service_bill` (`REQUEST_BILL`),
  `service_charge` (`CHARGE_TO_ROOM`) or `service_waiter` (anything else). A garbled
  argument is logged and read as 0 / empty, so the alert still plays.
- **Sound mode per RVC** (`mode`, `fetch`): `guest.speakerSoundMode` from
  `GET /api/v1/speaker/sound-mode/{rvc}`, read at start and every `soundRefreshSeconds`
  (default 120, at least 30). The Go rules are kept: a tone name plays that tone;
  `"tone": null` or `""` is "tone off" while the voice may still speak; a missing `tone`
  key or a malformed value is the default (tone `guest`, voice on); `lang` `en`/`en-us`/
  `en-gb` speaks English, anything else Vietnamese. A failed fetch keeps the last good
  mode; an RVC never fetched uses the default.
- **Output** (`output`, `sapi`, `tones`): the 11 Go tones are embedded; announcements
  play in arrival order on one worker thread (queue of 32, overflow dropped and counted),
  tone first, then speech. On Windows both go through SAPI 5 over `dl-winos` COM on a
  dedicated STA thread: the tone as a WAV file stream (`SAPI.SpFileStream` +
  `SpeakStream`, the files written once to `<data>/modules/speaker/tones`), the phrase with
  `SAPI.SpVoice.Speak` and the first installed voice of the language (LCID 042A for `vi`,
  0409 then 0809 for `en`; none installed: the default voice, as the Go agent did). Off
  Windows the output only logs.
- **Commands**: `status` (settings, joined/refused RVCs, modes, fetch failures, counters;
  no secrets) and `test` (`{"rvc": 1, "kind": "order", "table": "5"}`: plays what that
  event would, to check the speaker on site).

## Authentication (the enrolled identity)

The Go agent used a 365-day speaker-device JWT written into its `config.json` by the
install script. This module keeps no second bearer secret: it reads the `pda-sync`
module's enrolment on the same machine (`<data>/modules/pda-sync/agent.json` for the
cloud URL, its secret store for the Ed25519 key and the agent token) and exchanges it
through `POST /sync/v1/token` (signed, as `pda-sync` does) for a 15-minute access token,
refreshed ahead of expiry and dropped on a 401. So `dl-agent pda-enroll` must have run on
the machine; until then the module is degraded with that hint. The token never appears
in logs, URLs (it is sent only as the `Authorization` header) or command answers.

### Cloud change needed (PDAOrder, not changed here)

The PDA cloud sees this connection as a **SyncAgent** principal, and today it refuses it
in two places:

1. `OrderHub.JoinRvc` throws `FORBIDDEN_RVC` for `user.IsSyncAgent()` (ADR 0098 made the
   SyncAgent a print relay only). Needed: allow a SyncAgent to join the RVC group of any
   branch of its own tenant (the same set `PrintRelayBranches` returns), still refusing
   `JoinCheck` and `JoinKdsDashboard`.
2. `GET /api/v1/speaker/sound-mode/{rvc}` requires `ClerkOrSpeakerDevice`. Needed: accept
   the SyncAgent role there too, scoped to its tenant's RVCs, read-only, the one key
   `guest.speakerSoundMode` (as for speaker devices).

Until (1) ships the module stays degraded ("the PDA cloud refused JoinRvc ...", report
`SPEAKER_JOIN_REFUSED` once) and plays nothing; until (2) ships every RVC uses the default
mode (guest tone, Vietnamese voice) and `status` lists the 403 per RVC. No speaker-device
JWT path was added on purpose (a long-lived bearer file is what this port removes).

## Not ported

- **Print relay.** The Go agent relayed prints when `printerSubnets` was set. Here
  `pda-sync` is the branch's print relay (ADR 0098), and ADR 0026 allows one relay per
  branch, so this module never joins a print relay group and ignores `PrintJob`.
- **`ForceUninstall`.** The cloud sends it only to speaker-device connections it revokes;
  it is logged and ignored here (dl-agent owns uninstalling; disable `modules.speaker`).
- **Service, installer, `config.json`.** dl-agent hosts the module.

## Verified where

The decision rules, phrases, envelope decoding, settings, the fetch (local HTTP server),
the queue and the hub link (fake hub: negotiate, upgrade, handshake, `JoinRvc` per RVC,
refusals, 401) are unit- and integration-tested on any OS. Real audio (SAPI voices, the
tone stream on a speaker) and the live PDA cloud (after the cloud change) can only be
verified on a Windows machine at a site.
