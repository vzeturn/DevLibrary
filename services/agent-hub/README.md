# agent-hub

Control plane for `dl-agent`: the hub side of [`crates/dl-agent/PROTOCOL.md`](../../crates/dl-agent/PROTOCOL.md).
A Cloudflare Worker (TypeScript strict, Hono, D1, R2). This is the only non-Rust directory in the repo.

## What it does

| Area | Endpoints |
|---|---|
| Agents (§1–§5) | `POST /v1/enroll` (one-time code), then Ed25519-signed `POST /v1/heartbeat`, `POST /v1/reports`, `POST /v1/agent/selftest`, `POST /v1/agent/diagnostics?command_id=` (zip), `POST /v1/commands/:id/result`, `GET /v1/releases/:product/:channel`, `GET /v1/artifacts/:key` (R2 `ARTIFACTS`, never `diag/…`) |
| Admin (`Authorization: Bearer $ADMIN_TOKEN`) | `PUT /admin/tenants/:id`, `GET /admin/tenants`; `POST/GET /admin/enroll-codes`, `DELETE /admin/enroll-codes/:id`; `GET /admin/agents`, `PUT /admin/agents/:id/channel`, `POST /admin/agents/:id/revoke`, `PUT /admin/agents/:id/config`; `POST/GET /admin/commands`; `GET /admin/releases`, `PUT /admin/releases/:product/:version` (canary manifest as the raw body), `GET …/gate`, `POST …/promote`; `GET /admin/reports?tenant_id&severity&fingerprint&version`; `GET /admin/agents/:id/survey`; `PUT/GET/DELETE /admin/products/:product/site-requirements`, `GET /admin/products/:product/sites?tenant_id` (ready / not ready per site, with reasons); `POST/GET /admin/product-keys`, `DELETE /admin/product-keys/:id`; `GET /admin/agents/:id/selftest`; `POST/GET /admin/bench`, `DELETE /admin/bench/:id`; `GET /admin/diagnostics?agent_id&tenant_id&limit`, `GET /admin/diagnostics/:id/download`; `GET /admin/audit?since&actor&path_prefix&limit` |
| Product platforms (`Authorization: Bearer <product key>`) | `POST /products/:product/enroll-codes` `{external_id, name, site_id?, ttl_seconds?, demo?}` → code for tenant `<product>-<external_id>` (created on first use), agent starts with `modules.<product>.enabled` |
| Install (public, `no-store`) | `GET /install.ps1` (Windows installer, this hub as default), `GET /install/info/:product/:channel/:target` (`key=value` lines: version, sha256, size, `server_time`, download path), `GET /install/download/:product/:channel/:version/:target` (streams the R2 build of a published release, or redirects to an off-hub URL) |
| Bench (public, `no-store`) | `GET /bench/:token` — the bench script of a bundle (see "Bench (test PC)") |
| Status | `GET /admin` — read-only HTML (Vietnamese) of agents, canary releases with gate status, site readiness per product, and the latest self test per agent ("Tự kiểm tra") |

Notes:

- Manifests are stored and served **verbatim**; the hub never re-serialises or signs them. Promotion
  takes a separately signed stable manifest: `{"manifest": "<text>", "force"?: true, "reason"?: "…"}`.
  A failing gate answers `409 {"error":"gate_failed","checks":[…]}`; `force` needs a `reason`, both are recorded.
- Canary gate (`src/domain/gate.ts`, pure): `min_agents`, `soak`, `modules_running` (`disabled` counts as
  healthy), `no_new_errors` (error/critical fingerprints not seen on the previous release), `no_rollback`.
  Defaults come from `GATE_MIN_AGENTS` (1) and `GATE_SOAK_HOURS` (2). Version history and
  `update_check` follow the `product` an agent sends in its heartbeat (default `dl-agent`), so product
  agents built on the runtime (for example `azolo-agent`) have their own release line. The first release
  of a product has no previous version: `no_new_errors` then means no error/critical report at all.
- Revoking an agent makes every later signed call answer `401 revoked`; the agent wipes its identity and
  stops on that answer, so no `revoke` command has to be queued first.
- Enrolment codes are shown once; only `sha256(code)` is stored. Codes are case-insensitive.
  `POST /admin/enroll-codes` also answers `install_command`, the PowerShell one-liner for the site, and
  takes an optional `config`: the remote config the enrolled agent starts from (version 1).
- Product keys (`dlpk_…`, shown once, only the SHA-256 stored) let a product platform such as E-Invoice
  mint enrolment codes for its own customers without the admin token or Cloudflare Access. A key only
  reaches tenants named `<product>-…` and only enables its own module; an existing tenant is left as the
  admin set it (name, demo flag). Codes it mints are listed with `minted_by`.
- `install.ps1` (PowerShell 2.0+, run as Administrator): picks `windows7-*` on NT < 10 and x86/x64,
  refuses a clock more than 4 min off the hub (after one `w32tm /resync`), excludes the agent folders in
  Defender, checks size + SHA-256, removes older copies (other services running the same exe, leftover
  processes, a service pointing elsewhere), optionally declares and tests SQL Server first (`-SqlPrompt`
  for a server on another machine, or `-SqlName`/`-SqlConnection`) so the code is not spent on a site that
  cannot reach its database, enrolls unless `identity.json` exists (re-runs keep the identity; `-Reenroll`
  replaces it), runs an optional product setup sub-command (`-Setup`, secrets through env
  `DL_AGENT_SETUP_ARGS`, removed afterwards), only then removes a product's previous agent (`-Replace
  '<service pattern>'`: services stopped and deleted, their processes ended, files kept, so any failure
  earlier leaves the old agent running), installs the service + watchdog, prints Kaspersky allow-list guidance with the exe hash, and
  waits for `heartbeat.json` to show a hub contact (`Connected`, or the last log lines and `status`).
- Self test: `dl-agent selftest` uploads `{run_id, trigger, started_at, finished_at, agent_version, os,
  hostname, checks:[{module, id, title, status: pass|warn|fail|skip, detail, remedy?, duration_ms}]}` to
  `POST /v1/agent/selftest`, signed like `/v1/reports` (at most 256 KB and 500 checks; a retried `run_id`
  replaces its run; the newest 20 runs per agent are kept). `GET /admin/agents/:id/selftest` answers the
  latest run with its checks and the summaries of the kept runs. To run it remotely, queue the generic
  command: `POST /admin/commands {"agent_id": "…", "kind": "selftest", "args": {"trigger": "command"}}`;
  the agent gets it on its next heartbeat like any other command (the hub has no special case for it).
- Diagnostics: the `collect_diagnostics` command makes the agent upload a zip (`diagnostics.json`, the
  newest logs within 6 MB, the state files, `bundle.json`; every text scrubbed of secrets on the agent,
  `identity.key` and `sql.json` never included) to `POST /v1/agent/diagnostics`. The hub refuses a body
  over 8 MB or without `Content-Length` before checking the signature, stores the zip in R2 as
  `diag/<tenant>/<agent>/<unix ms>-<8 hex>.zip` and a `diagnostics` row (agent, tenant, site, size,
  SHA-256, command id, agent version), and answers `{id, key, size, sha256}`; the command result carries
  the same. Bundles are never served on `/v1/artifacts/*`; admins list and download them. Nothing prunes
  them yet.
- Admin audit: every admin request that changes something (any method but GET/HEAD, after auth) is
  written to `admin_audit` before it runs — time, actor (`Cf-Access-Authenticated-User-Email`, else
  `admin-token`), method, path with query, body with secrets replaced by `[redacted]` (JSON fields named
  like password/secret/token/api key/private key/authorization/`*_sealed`, `Password=` in connection
  strings, `Bearer`/`Basic` values; cut at 4 000 characters) — and its status and error code once it
  finished. If the row cannot be written the change is refused with `503 audit_unavailable`.
- A cron trigger prunes the nonce replay table every 15 minutes.

## Bench (test PC)

One line sets up a test PC with dl-agent on canary, the product modules and a self test:

```sh
curl -sS https://agent.pms.id.vn/admin/bench -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "CF-Access-Client-Id: …" -H "CF-Access-Client-Secret: …" -H 'Content-Type: application/json' -d '{
  "tenant_id": "bench-hiep", "site_id": "test-pc", "label": "SMILE restore on the test PC",
  "sql_databases": ["SMILE_FO"],
  "sql_fixups": [{"name": "quiet outbound", "database": "SMILE_FO", "sql_url": "https://…/fixups.sql", "sha256": "…"}],
  "steps": [
    {"setup": "einvoice-enroll", "args": ["--platform-url", "https://…", "--state-dir", "{{BENCH_DIR}}\\einvoice"],
     "env": {"DL_AGENT_EINVOICE_TOKEN": "…", "MMEI_SQL_HOST": "{{SQL_SERVER}}", "MMEI_SQL_USER": "{{SQL_USER}}",
             "MMEI_SQL_PASSWORD": "{{SQL_PASSWORD}}", "MMEI_SQL_AUTH": "{{SQL_AUTH}}"}},
    {"setup": "membership-enroll", "env": {"DL_MEMBERSHIP_SQL": "{{SQL:SMILE_POS}}"}}
  ],
  "config": {"modules": {"einvoice": {"enabled": true}}},
  "expires_hours": 24, "max_fetches": 10 }'
# → 201 {"id": "<sha256 of the token>", "irm": "irm https://agent.pms.id.vn/bench/<token> | iex", "expires_at": …}
```

- The tenant is created as a **demo** tenant (its agents follow canary); an existing tenant that is not
  a demo answers `409 tenant_not_demo` (the hub never moves a live tenant to canary). An enrolment code
  for the tenant/site is minted with the bundle's lifetime (`expires_hours` 1–72, default 24) and the
  optional `config` as the agent's first remote config.
- The link token (256 bits, base64url) is shown once; D1 keeps its SHA-256 as the bundle `id`. Steps,
  env values, fixups and the code are sealed with AES-256-GCM under the `BENCH_KEY` secret (the id is
  the AAD); without that secret every bench call answers `503 bench_key_missing`.
- `GET /bench/<token>` answers `text/plain`, `Cache-Control: no-store`, and counts the fetch atomically;
  unknown, expired, revoked or used-up links (`max_fetches`, default 10) all answer the same 404.
  `GET /admin/bench` lists bundles (`active`, `expired`, `revoked`, `used_up`, whether the code was
  used), never the sealed steps; `DELETE /admin/bench/:id` revokes a bundle and its unused code.
- On the PC, in Windows PowerShell 5.1 as Administrator, `irm … | iex`:
  1. removes the older per-product agents (`remove_legacy`, default `true`): scheduled tasks whose
     name, path or action matches the legacy pattern (E-Invoice, SmilePmsAgent, SmileHLAgent,
     MM.Arrival.Agent and its watchdog task, MM.Membership.Agent, SRBackup, PdaSyncAgent) are
     unregistered (`schtasks /Delete /F` when the ScheduledTasks module is missing); matching services
     are stopped (≤ 30 s, then their process is killed) and `sc.exe delete`d; their data and install
     folders (one table in `src/install/benchLegacy.ts`, each entry citing the installer it comes from,
     plus the folder of each removed service's binary) are **renamed** to `<dir>.bench-old-<yyyyMMddHHmmss>`,
     never deleted. `dl-agent`, SQL Server services (also a named instance such as `MSSQL$SMILEPMS`) and
     anything unmatched are never touched. Every action is printed and in the summary, and recorded in
     `%ProgramData%\dl-agent\bench-removed-<ts>.json` (with a one-line restore hint: rename each `to` back
     to `from`, then run that agent's own installer). The first failure stops the bench before
     install, so the code is not spent. With `"remove_legacy": false` (and for bundles sealed before the
     field existed) they are only listed, never stopped or replaced, and compared again at the end;
  2. asks once for SQL Server (`.` by default, SQL login or Windows) when the bundle names a database
     or a step uses a SQL part placeholder,
     unless `-SqlConnection` is passed (`& ([scriptblock]::Create((irm <link>))) -SqlConnection '…'`),
     and checks each database exists;
  3. runs each `sql_fixups` entry (`sql_inline`, or `sql_url` with an optional `sha256`) once against its
     database in one transaction, `GO` lines splitting batches; only names and row counts are printed. A
     failed fixup stops the bench before install, so nothing runs against a copy that may still reach live
     services;
  4. runs the hub `install.ps1 -Code <code> -Channel canary`, then `dl-agent sql-add`/`sql-test` per
     `sql_databases` (the connection only in `DL_AGENT_SQL_CONNECTION`), then each step as
     `dl-agent <setup> <args…> --data %ProgramData%\dl-agent` with its env set for that process only.
     `{{SQL:<Db>}}` (env only) becomes the connection string for that database; `{{SQL_SERVER}}`,
     `{{SQL_USER}}`, `{{SQL_PASSWORD}}` and `{{SQL_AUTH}}` (`sql` or `windows`; env only) give the
     prompted connection field by field (user and password empty with Windows); `{{BENCH_DIR}}`
     (args and env) becomes `%ProgramData%\dl-agent\bench`, so steps keep their own install/config/state
     directories away from the older agents'. A failed step is reported and the next one runs;
  5. restarts the service, waits for a hub contact, runs `dl-agent selftest --trigger install` (skipped
     with a message on a build without it) and prints the summary table.
  The log goes to `%ProgramData%\dl-agent\bench-<date>.log`; the code, env values and the SQL
  password are masked in the console and the log.

## Develop

```sh
pnpm install
pnpm typecheck && pnpm test     # vitest on node:sqlite (D1 look-alike running the real migrations)
pnpm dev                        # wrangler dev
```

Layering: `routes/` (HTTP, validation) → `services/` (rules) → `repo/` (D1 SQL, parameterised);
`domain/` holds pure logic (gate, channel, schemas, version compare).

## Production

Live at **https://agent.pms.id.vn** since 2026-11-18 (owner chose the host, RT-0164): Worker
`agent-hub`, D1 `agent-hub-db`, R2 `agent-hub-artifacts`, account and IDs in `wrangler.jsonc`.

- `/v1/*` is public; every call is Ed25519-signed by the agent.
- `/admin*` sits behind Cloudflare Access (app "DevLibrary agent hub admin", service-token policy)
  *and* the `ADMIN_TOKEN` Bearer. Admin calls send both: `Authorization: Bearer …`,
  `CF-Access-Client-Id` and `CF-Access-Client-Secret`.
- Operator secrets live outside git on the owner's iMac: `~/.config/devlibrary-agent-hub/admin.env`
  (admin token, Access service token) and `release.seed` (offline release signing key,
  public half `rel-2026:jz7z7AVXs8ArKexwiZimYtGICV9cXrie6v7Cuulw3CI=` baked into builds with
  `DL_AGENT_RELEASE_KEYS`).
- Redeploy: `pnpm typecheck && pnpm test`, then `pnpm db:migrate` (remote) and `pnpm deploy`.
- Bench needs `BENCH_KEY` (32 random bytes, base64): `openssl rand -base64 32 | wrangler secret put BENCH_KEY`.
  Rotating it makes every existing bundle unreadable (`500 bench_unreadable`); revoke them first.
- Artifacts: `wrangler r2 object put agent-hub-artifacts/<product>/<version>/<target>/<file> --remote`,
  served to agents at `/v1/artifacts/<key>`.
