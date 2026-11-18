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

## 9. SQL Server bridge (on-prem)

The agent is the one bridge between hub-side projects and the site's SQL Server databases.

- **Declaration stays local.** `<data>/sql.json` = `{"databases":{"<name>":{"connection":"…",
  "max_connections":4,"query_timeout_seconds":15,"acquire_timeout_seconds":10,
  "idle_timeout_seconds":300,"max_lifetime_seconds":1800,"lock_timeout_seconds":8,
  "transport":"auto"}}}`. Names are 1–64 of
  `[A-Za-z0-9_-]`; connection strings use the ADO (`Server=…;Database=…`) or `sqlserver://`
  form and never leave the machine (file mode 0600 / service-account ACL). `dl-agent sql-add
  <name> --connection …` (or env `DL_AGENT_SQL_CONNECTION`) writes an entry after validating
  it; `dl-agent sql-test [name]` checks connectivity; changes apply on the next restart.
- **Invalid file**: the agent runs without SQL and files one `sql_config_invalid` (critical)
  report; error texts never echo the connection string.
- **Pools**: one per database, shared by every module (`ModuleContext::sql(name)`). Idle
  sessions are reused newest first and pinged when idle ≥ 30 s; a session that is broken,
  still inside a transaction, or past its lifetime is closed instead of reused; callers wait
  up to `acquire_timeout` for a free session. A database that refuses connections trips a
  breaker: callers fail fast with "database unavailable" and one caller probes after a
  backoff (1 s doubling to 60 s). Each query carries `query_timeout` (0 = none); a timeout
  cancels the request with an attention and keeps the session.
- **Transport ladder** (`transport`, local only): with the default `encrypt=false` (TLS for
  the login) and `transport: "auto"`, a failed connect steps down *as configured* → *TLS 1.0
  minimum* → *unencrypted* when the TLS handshake fails at the server or the server resets /
  closes an accepted connection (SQL Server 2008 R2 RTM completes TLS 1.0 and then resets).
  A refused or timed-out dial, a SQL error (login, database), a certificate error or a client
  without TLS never steps down. The first rung that connects is kept until restart; later
  drops are outages. `encrypt=true|strict|disable` never steps down; `transport` may pin
  `as_configured`, `tls1.0` or `unencrypted`. Ending unencrypted files one `sql_unencrypted`
  (warning); the rung is in stats (`transport.rung`) and in `sql-test` output.
- **Lock waits**: every new session runs `SET LOCK_TIMEOUT` (`lock_timeout_seconds`, default
  8, 0 = wait forever), so a row locked by the product fails fast with SQL error 1222
  instead of holding a slot until `query_timeout` (default 15 s, below the hub's 20 s wait).
- **Hub tuning**: remote config `body.sql = {"<name>": {"max_connections": 8, …}}` overrides
  the limits per database; a `connection` key there is ignored. Removing the override restores
  the local values.
- **`sql` module** (present only when databases are declared): checks every database each
  30 s, is `degraded` while one fails (so the canary gate blocks a build that breaks SQL),
  files `sql_unreachable` once per outage and `sql_recovered` on recovery (context
  `{"database"}`), and publishes pool stats as the `databases` metric.
- **Commands**: `sql.ping {database?}` → `{"<name>":{"ok":true,"ms":3}}`; `sql.status` → pool
  stats (`open`, `idle`, `in_use`, `max`, `connects`, `connect_failures`, `acquires`, `waits`,
  `timeouts`, `discarded`, `down_since`, `last_error`, `transport {rung, lock_timeout_ms}`). There is deliberately no command that
  runs arbitrary SQL sent by the hub: projects reach their databases through their own
  modules.
