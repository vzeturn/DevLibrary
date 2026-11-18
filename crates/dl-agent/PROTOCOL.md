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
             "detail":"…","restarts":0,"last_error":"…","metrics":{"rows_out":120}}],
 "survey":{"host":{…},"databases":{"SMILE_FO":{…}}}}
```

`survey` (§9.5) is present on the first heartbeat after start, whenever it changes (a
database answered for the first time, a daily re-survey found new grants) and at least
once a day; the hub keeps the last one it received. A malformed or oversized (> 256 KiB)
survey is dropped and logged; the heartbeat itself is still accepted.

`product` names the release line the agent follows (default `dl-agent`); product agents
built on the runtime send their own name. The hub stores the latest heartbeat per agent; an agent with no heartbeat for
3 × `heartbeat_seconds` is shown as offline.

## 4. Commands (hub → agent)

`{id, kind, module?, args:{…}, expires_at}`. Built-in kinds handled by the runtime:
`restart`, `restart_module`, `update_now` (ignores the site's window and `auto: false`, not
the failure cap; the output says why nothing was installed), `retry_update {version?}`
(forgets failed attempts of one version or all, §6.2), `set_log_level`,
`collect_diagnostics`, `reload_config`, `revoke` (wipe identity and stop). Any other kind with a `module` is
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
version arrived during the soak, no agent rolled the version back, and, when the product has
site requirements (§9.5), every canary site running the version meets them (`site_eligible`). Stable releases
carry `rollout_percent`; an agent installs only when
`sha256(agent_id + "|" + product + "|" + version)[0..2] as u16 % 100 < rollout_percent`.

Manifest (signed with an offline Ed25519 release key; agents ship the trusted public keys):

```json
{"product":"dl-agent","channel":"stable","version":"1.4.0","published_at":1763460000,
 "rollout_percent":100,"min_version":"1.0.0","allow_downgrade":false,"notes":"…",
 "artifacts":[{"target":"windows-x86_64","url":"https://…","sha256":"<hex>","size":123}],
 "key_id":"rel-2026","signature":"<b64>"}
```

`min_version` is `null` when the release has no minimum. Two optional flags, written only
when `true` so older signatures stay valid: `"urgent":true` installs outside the site's
update window (a fix that cannot wait for the night), `"required":true` installs even where
the site turned automatic updates off. Sign with `release sign … --urgent --required`.
The signature covers the canonical JSON of every field except `signature`, in the order
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
   probation, the agent restores `previous`, counts a failure of that version, sends a
   `critical` report `update_rolled_back`, and restarts.

### 6.1 Site update policy

The remote configuration (§8) may carry
`"update": {"auto": true, "window": {"from": "01:00", "to": "05:00", "utc_offset_minutes": 420}}`.
`auto: false` holds every release that is not `required`; outside the window (local time,
`from == to` means all day, may cross midnight; offset defaults to UTC+7) every release that
is not `urgent` waits. A held release sets the update status `deferred` with the reason and
is checked again at the next update check. An invalid section is reported once
(`update_policy_invalid`, warning) and treated as "no window".

### 6.2 Failure cap

A failure is a bad download (size or SHA-256 mismatch: a stale CDN copy, a truncated file),
a failed swap, or a rollback. Network errors are not failures, so a hub outage never holds
an urgent fix. After 3 failures of one version in 24 h the agent waits until the oldest of
them is a day old (`deferred`); after 9 it stops trying that version for good (`update_failed`
with "send retry_update"). `retry_update` clears the count. Downloads from a URL outside
the hub send `Cache-Control: no-cache`; the hub serves artifacts with `no-store`.

### 6.3 External watchdog

The agent rewrites `heartbeat.json` (`{at, hub_at, version, pid}`) once a minute and at
once after each successful hub heartbeat. `install` registers `<binary> watchdog`, run every
5 minutes: a scheduled task `"<display name> Watchdog"` as SYSTEM on Windows (put back and
service recovery re-applied at each service start), a `<service>-watchdog.timer` on Linux.
Each run takes at most one action:

| Service | Condition | Action |
|---|---|---|
| missing or disabled | | none (someone turned it off) |
| stopped | | start |
| starting/stopping | for 10 min | kill the pid from `heartbeat.json`, start |
| running | an update is on probation | none (the agent rolls back itself) |
| running | loop beat ≥ 20 min old | restart |
| running | hub beat ≥ 20 min old **and** the hub `/healthz` answers | restart |

Restarts back off 1 h, 2 h, 4 h … 12 h, reset by a healthy run. Every action leaves a report
(`watchdog_started` warning, `watchdog_killed` / `watchdog_restarted` error,
`watchdog_action_failed` critical) in `watchdog-reports.json`; the agent sends them after
its next start. The watchdog logs to `logs/watchdog.log`; `uninstall` removes the task or
timer first.

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
  files `sql_unreachable` once per outage (`sql_access_denied` when the login or database is
  refused) and `sql_recovered` on recovery (context `{"database"}`), and publishes pool
  stats as the `databases` metric.
- **Commands**: `sql.ping {database?}` → `{"<name>":{"ok":true,"ms":3}}`; `sql.status` → pool
  stats (`open`, `idle`, `in_use`, `max`, `connects`, `connect_failures`, `acquires`, `waits`,
  `timeouts`, `discarded`, `down_since`, `last_error`, `transport {rung, lock_timeout_ms}`);
  `sql.survey {database?}` → `{"host":…,"databases":{"<name>":survey}}` (re-runs it now);
  `sql.probe {requirement, database?}` → `{"<name>":probe}` (§9.6);
  `sql.install {bundle, database?, dry_run?, force?}` → `{"<name>":install}` (§9.7). There is
  deliberately no command that runs arbitrary SQL sent by the hub: a bundle must carry a
  release signature, and projects otherwise reach their databases through their own modules.

### 9.5 Site survey

So the hub can tell a site that cannot run a product before it is moved, the agent
describes its host and every declared database. Facts only, no data and no secrets.

- **`host`** (taken once at start): `family`, `target`, `version` (`ver` output on Windows,
  `PRETTY_NAME (kernel …)` on Linux), `nt_version` (`6.1.7601`), `legacy_windows` (NT < 10:
  needs a `windows7-*` build), `process_arch`, `os_arch` (differs when a 32-bit build runs
  on 64-bit Windows), `win7_build`.
- **Per database** (on the first successful check, then every 24 h; one parameterless query
  using built-ins that exist on SQL Server 2005+): `engine {version, major, name ("SQL
  Server 2008 R2"), level, edition, engine_edition, host_os}`, `database {name,
  compatibility_level, collation, code_page, read_only}`, `server_collation`, `login`,
  `user`, `grants {sysadmin, db_owner, create_procedure, create_function, create_table,
  alter_dbo, execute, view_definition, backup_database}`, `syntax_2012` (engine ≥ 11 and
  compat ≥ 110: `THROW`, `OFFSET`, `IIF`, … accepted), `transport` (rung) and `warnings`:

  | warning | meaning |
  |---------|---------|
  | `engine_pre_2012` | SQL Server 2008 R2 or older: T-SQL 2008 only |
  | `compat_below_2012` | newer engine, but compat < 110 still refuses 2012 syntax |
  | `collation_differs_from_server` | `#temp` joins need `COLLATE DATABASE_DEFAULT` |
  | `non_vietnamese_code_page` | `varchar` holds ANSI / TCVN3 bytes, not CP1258 |
  | `cannot_install_procedures` | neither owner nor CREATE PROCEDURE + ALTER on dbo |
  | `no_execute_grant` | neither owner nor EXECUTE |
  | `database_read_only` | database is read-only |
  | `express_edition` | 10 GB data cap, no SQL Agent |
  | `unencrypted_transport` | reached on the unencrypted rung |

  A failed survey files `sql_survey_failed` (warning) and is retried the next day.

- **Site requirements** (hub, per product): `{"host": {"min_nt_version": "6.1", "families":
  ["windows"]}, "databases": {"SMILE_FO": {"min_engine_major": 10, "min_compatibility": 100,
  "grants": ["execute", "create_procedure"], "forbid_warnings": ["database_read_only"],
  "probe_ok": true}}}`. A site is ready when its last survey (at most 3 days old) meets every
  item; `sysadmin` or `db_owner` satisfies any grant, `probe_ok` needs the database's last
  `probe` (§9.6) to have nothing missing. The hub lists ready and unready sites before a site
  is moved onto a product, and the canary gate refuses a release whose canary sites are not ready.

### 9.6 Schema probe and SQL error codes

A product names what it needs from a database; `sql.probe` answers what is missing, so a
site is fixed (or its install bundle runs) before a sync fails on "Invalid column
name" in production.

```json
{"requirement": {
  "tables": {"dbo.HoaDon": ["Id", "SoHD", "MaCQT"], "dbo.Config": []},
  "views": ["dbo.vw_Bill"], "procedures": ["dbo.usp_GetBill"], "functions": ["dbo.fn_Tcvn3"],
  "rows": [{"table": "dbo.Config", "column": "Key", "values": ["EINV_URL"]}]},
 "database": "SMILE_FO"}
```

- Names are `name` (schema `dbo`) or `schema.name`, brackets allowed, compared
  case-insensitively; a synonym satisfies any kind; at most 1000 items.
- Answer per database: `{"ok", "checked": n, "missing": [{kind, name, found?, value?}]}`
  with `kind` `table` / `view` / `procedure` / `function` / `column` / `row`; `found` names
  the kind that exists instead (a table where a view is expected). Columns of a missing
  table and rows of a missing table or column are not listed twice.
- Catalog queries use `sys.objects`, `sys.columns` and `OBJECT_ID` only (SQL Server 2005+);
  names and row values are bound as parameters, never spliced.
- The last answer is kept as `probe` inside that database's survey, so the next heartbeat
  carries it to the hub gate.

SQL errors from a session carry a code prefix the agent and modules act on:

| code | SQL error | meaning |
|------|-----------|---------|
| `sql_missing_object` | 207, 208, 2812, 4121 | schema differs from what the code expects |
| `sql_locked` | 1222 | lock wait exceeded `LOCK_TIMEOUT`; retry later |
| `sql_deadlock` | 1205 | deadlock victim; retry |
| `sql_access_denied` | 18456, 4060, 229, 230 | login, database or permission refused |
| `sql_timeout` | (client) | request timed out and was cancelled |

### 9.7 Signed SQL bundles

The procedures, functions, views and idempotent scripts a product needs in a site database
travel as one bundle signed offline with a release key (`release sign-sql <seed> <key-id>
<bundle.json>`; an object may name a `file` instead of inline `sql`). The agent verifies it
against its baked-in release keys, so neither the hub nor the network can push SQL.

```json
{"product": "einvoice", "version": "1.4.0", "published_at": 1763500000,
 "objects": [
   {"kind": "script", "name": "dbo.add_MaCQT",
    "sql": "IF COL_LENGTH('dbo.HoaDon','MaCQT') IS NULL ALTER TABLE dbo.HoaDon ADD MaCQT varchar(50) NULL"},
   {"kind": "procedure", "name": "dbo.usp_GetBill",
    "sql": "CREATE OR ALTER PROCEDURE dbo.usp_GetBill @Id int AS …"}],
 "requires": {"procedures": ["dbo.usp_GetBill"], "tables": {"dbo.HoaDon": ["MaCQT"]}},
 "key_id": "rel-2026", "signature": "<base64 Ed25519 over the compact JSON of the rest>"}
```

- `kind` is `procedure`, `function`, `view` or `script`; an object's batch must create
  exactly the entry's kind and name (checked when parsed). At most 500 objects.
- **Old engines.** The site level is `min(engine major × 10, compatibility level)`. Before
  any change, every batch is scanned (comments and strings skipped) for syntax above it:
  2012 (`IIF`, `TRY_CONVERT`, `CONCAT`, `FORMAT`, `THROW`, `OFFSET … FETCH`, sequences…),
  2016 (`STRING_SPLIT`, `OPENJSON`, `DROP … IF EXISTS`, `CREATE OR ALTER` inside a script),
  2017 (`STRING_AGG`, `TRIM`, `CONCAT_WS`) and 2022 (`GREATEST`, `DATETRUNC`); the bundle is
  refused (`refused: [{name, features}]`). An object's own `CREATE OR ALTER` header is
  rewritten to `CREATE` (missing) or `ALTER` (present), so grants and properties survive.
- **Backup.** Each object that will be replaced has its `OBJECT_DEFINITION` written to
  `sql-backups/<database>/<product>-<version>-<time>/<schema>.<name>.sql` first. A
  definition that cannot be read (`WITH ENCRYPTION`, no `VIEW DEFINITION`) refuses the
  bundle (`unreadable`) unless `force`.
- **All or nothing.** Scripts and objects run in order inside one transaction; then
  `requires` is probed (§9.6) in the same transaction. Any failure (`failed: {kind, name,
  code, error}`) or an unmet requirement rolls everything back. `dry_run` always rolls back
  and reports what would have happened.
- **Ledger.** Each installed object gets the extended property `dl_bundle =
  "product@version sha256"`; an object already carrying the same stamp is `unchanged` and
  skipped, so re-sending a bundle repairs drift without touching what is right. Real
  installs are appended to `sql-installs.jsonl`, and the probe joins the survey.
- Answer per database: `{"ok", "dry_run", "level", "applied": [{kind, name, action:
  script|create|alter}], "unchanged": [name], "backups": [path], "failed"?, "refused"?,
  "unreadable"?, "probe"?, "error"?}`.
- Modules call `install::with_repair(conn, bundle, …, f)`: when `f` fails with
  `sql_missing_object`, the bundle is verified and installed and `f` runs once more.
