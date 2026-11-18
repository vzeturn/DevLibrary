# dl-agent protocol, version 1

One on-prem agent per site serves every product installed there (PDA Order, PMS, E-Invoice,
Check-In, Membership, Hotel Link Sync, SRBackup …). Product features are **modules** inside
the agent. Business data still flows between each module and its product's own cloud; the
agent's **lifecycle** (enrolment, heartbeat, remote commands, problem reports, updates) goes
to one shared control plane, the **agent hub**.

All bodies are UTF-8 JSON. Times are Unix seconds (UTC). Unknown fields must be ignored by
both sides, so either side can add fields without a version bump.

## 1. Identity and request signing

At enrolment the agent creates an Ed25519 key pair. The seed never leaves the machine
(Windows: DPAPI, machine scope, file ACL owner-only; elsewhere: file mode 0600). Every call
after enrolment is signed:

```
X-Agent-Id:        <agent_id>
X-Agent-Time:      <unix seconds>
X-Agent-Nonce:     <16 random bytes, hex>
X-Agent-Signature: base64(ed25519(seed, canonical))

canonical = "DLAGENT1\n" + METHOD + "\n" + path_and_query + "\n" + time + "\n" + nonce
            + "\n" + hex(sha256(body))
```

The hub rejects a request when the agent is unknown or revoked, the time is more than
300 s away from the hub clock (answer `401` with `{"error":"clock_skew","server_time":N}` so
the agent can report the skew), the nonce was seen in the last 10 minutes, or the signature
does not verify.

## 2. Endpoints (agent → hub)

| Method & path | Body → Answer |
|---|---|
| `POST /v1/enroll` (unsigned; the one-time code authenticates) | `{code, public_key (b64), hostname, os, arch, version, modules:[name]}` → `{agent_id, tenant, site, channel, heartbeat_seconds, config:{version, body}}` |
| `POST /v1/heartbeat` | Heartbeat (§3) → `{server_time, channel, heartbeat_seconds, config?:{version, body}, commands:[Command], update_check:bool}` |
| `POST /v1/reports` | `{reports:[ProblemReport]}` → `{accepted:N}` |
| `POST /v1/commands/{id}/result` | `{ok:bool, output:string, finished_at}` → `{}` |
| `GET  /v1/releases/{product}/{channel}` | → signed Manifest (§5); `404` when no release |
| `GET  <artifact url>` | binary, size and SHA-256 come from the manifest |

## 3. Heartbeat

```json
{"agent_id":"…","product":"dl-agent","version":"1.4.0","channel":"canary","target":"windows-x86_64",
 "hostname":"FO-SERVER","started_at":1763460000,"uptime_seconds":3600,
 "config_version":7,"update":{"state":"idle|probation|rolled_back","version":"1.4.0"},
 "modules":[{"name":"pda-sync","state":"running|starting|degraded|stopped|disabled",
             "detail":"…","restarts":0,"last_error":"…","metrics":{"rows_out":120}}]}
```

`product` names the release line the agent follows (default `dl-agent`); product agents
built on the runtime send their own name. The hub stores the latest heartbeat per agent; an agent with no heartbeat for
3 × `heartbeat_seconds` is shown as offline.

## 4. Commands (hub → agent)

`{id, kind, module?, args:{…}, expires_at}`. Built-in kinds handled by the runtime:
`restart`, `restart_module`, `update_now`, `set_log_level`, `collect_diagnostics`,
`reload_config`, `revoke` (wipe identity and stop). Any other kind with a `module` is
passed to that module. Expired commands are answered `{ok:false, output:"expired"}`.
Every command gets exactly one result.

## 5. Releases, channels and the canary gate

Channels: `canary` and `stable`. The hub decides an agent's channel:

- agents of **demo tenants** (tenant flag `demo`) and agents marked `canary` are on `canary`;
- everyone else is on `stable`.

A release is published to `canary` first. It may be promoted to `stable` only when the
**canary gate** passes: at least `min_agents` canary agents (default 1) have reported that
version for at least `soak_hours` (default 2), every module on them is `running` (or
`disabled` by config, an operator choice rather than a fault), no
problem report with severity `error`/`critical` and a fingerprint not seen on the previous
version arrived during the soak, and no agent rolled the version back. Stable releases
carry `rollout_percent`; an agent installs only when
`sha256(agent_id + "|" + product + "|" + version)[0..2] as u16 % 100 < rollout_percent`.

Manifest (signed with an offline Ed25519 release key; agents ship the trusted public keys):

```json
{"product":"dl-agent","channel":"stable","version":"1.4.0","published_at":1763460000,
 "rollout_percent":100,"min_version":"1.0.0","allow_downgrade":false,"notes":"…",
 "artifacts":[{"target":"windows-x86_64","url":"https://…","sha256":"<hex>","size":123}],
 "key_id":"rel-2026","signature":"<b64>"}
```

`min_version` is `null` when the release has no minimum. The signature covers the canonical JSON of every field except `signature`, in the order
above, with artifacts in the order given. Agents refuse a manifest that is unsigned, signed
by an unknown key, older (`published_at`) than the last one they accepted on that channel,
or lower in version than the running one unless `allow_downgrade` is true.

## 6. Update, probation and rollback (agent side)

1. Download the artifact for the agent's target, check size and SHA-256, stage it.
2. Keep the running binary as `previous`, put the new one in place, record
   `pending = {version, previous, probation_until}` and exit with code 75; the service
   manager restarts the agent.
3. On each start during probation the agent counts the boot. If it reaches the probation end
   with all modules running and one successful heartbeat, the update is committed.
4. If the process starts 3 times without committing, or a module stays degraded through
   probation, the agent restores `previous`, marks the version as failed (it will not be
   tried again), sends a `critical` report `update_rolled_back`, and restarts.

## 7. Problem reports

```json
{"fingerprint":"9f2c…","severity":"info|warning|error|critical","module":"pda-sync",
 "code":"sql_login_failed","message":"…","context":{"server":"FO\\SQL2019"},
 "first_seen":1763460000,"last_seen":1763463600,"count":12,
 "agent_version":"1.4.0","os":"windows","hostname":"FO-SERVER"}
```

The fingerprint is `hex(sha256(module|code|message with digits removed))[0..16]`, so the
same fault coalesces. Reports are kept in an on-disk spool (bounded) while the hub is
unreachable and sent oldest first.

## 8. Remote configuration

`config = {version, body}` where `body = {"modules":{"<name>":{"enabled":true,…}},
"log_level":"info"}`. The agent applies a newer version atomically, restarts only the modules
whose section changed, and keeps the previous version as last-known-good: if a restarted
module fails within 2 minutes the agent restores last-known-good and reports
`config_reverted`.
