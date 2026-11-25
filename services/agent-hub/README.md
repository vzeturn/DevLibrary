# agent-hub

Control plane for `dl-agent`: the hub side of [`crates/dl-agent/PROTOCOL.md`](../../crates/dl-agent/PROTOCOL.md).
A Cloudflare Worker (TypeScript strict, Hono, D1, R2). This is the only non-Rust directory in the repo.

## What it does

| Area | Endpoints |
|---|---|
| Agents (§1–§5) | `POST /v1/enroll` (one-time code), then Ed25519-signed `POST /v1/attach` (another product's code on the same agent, PROTOCOL §2.3), `POST /v1/heartbeat`, `POST /v1/reports`, `POST /v1/agent/selftest`, `POST /v1/agent/diagnostics?command_id=` (zip), `POST /v1/commands/:id/result`, `GET /v1/releases/:product/:channel`, `GET /v1/artifacts/:key` (R2 `ARTIFACTS`, never `diag/…`) |
| Admin (`Authorization: Bearer $ADMIN_TOKEN`) | `PUT /admin/tenants/:id`, `GET /admin/tenants`; `POST/GET /admin/enroll-codes`, `DELETE /admin/enroll-codes/:id`; `GET /admin/agents?tenant_id&include_revoked` (live machines only; `include_revoked=1` also lists revoked ones), `GET /admin/agents/:id` (finds revoked agents too) (each with its `products` links), `PUT /admin/agents/:id/channel`, `POST /admin/agents/:id/revoke`, `PUT /admin/agents/:id/config`, `POST /admin/agents/:id/attach` (remote attach: enable a product module and run its enrol tool on the machine, PROTOCOL §4.2; for einvoice, arrival, membership, pms-site (`config_path`, `state_dir`) and pms-device (`appsettings_dir`, tool `pms-device-enroll`) the hub adds the paths the agent config names for the module, so the tool writes where the module reads; `reenroll` with a new `external_id` moves the product link; arrival-enroll without a connection string uses the core's saved SMILE_FO); `POST/GET /admin/commands`; `GET /admin/releases`, `PUT /admin/releases/:product/:version` (canary manifest as the raw body), `GET …/gate`, `POST …/promote`; `GET /admin/reports?tenant_id&severity&fingerprint&version`; `GET /admin/agents/:id/survey`; `PUT/GET/DELETE /admin/products/:product/site-requirements`, `GET /admin/products/:product/sites?tenant_id` (ready / not ready per site, with reasons); `POST/GET /admin/product-keys`, `DELETE /admin/product-keys/:id`; `GET /admin/agents/:id/selftest`; `POST/GET /admin/bench`, `DELETE /admin/bench/:id`; `POST /admin/demo-bench`; `GET /admin/diagnostics?agent_id&tenant_id&limit`, `GET /admin/diagnostics/:id/download`; `GET /admin/audit?since&actor&path_prefix&limit`; `PUT/GET /admin/test-runs/:suite?limit` |
| Ops console (`Authorization: Bearer $MCP_TOKEN`) | `POST /mcp` (MCP, JSON-RPC): read-only tools for the Request Tracking assistant, see below |
| Product platforms (`Authorization: Bearer <product key>`) | `POST /products/:product/enroll-codes` `{external_id, name, site_id?, ttl_seconds?, demo?}` → code for tenant `<product>-<external_id>` (created on first use), agent starts with `modules.<product>.enabled` |
| Install (public, `no-store`) | `GET /install.ps1` (Windows installer, this hub as default), `GET /install/info/:product/:channel/:target` (`key=value` lines: version, sha256, size, `server_time`, download path), `GET /install/download/:product/:channel/:version/:target` (streams the R2 build of a published release, or redirects to an off-hub URL) |
| Guided install (public, `no-store`) | `GET /deploy#code=…` (the page), `POST /deploy/status {code}` (that code's progress), see "Guided install page" |
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
  cannot reach its database, chooses the SQL login (`[1] Windows [2] SQL login`, or unattended
  `-SqlAuth windows|sql -SqlServer -SqlUser` with the password only from env
  `DL_AGENT_SQL_PASSWORD` or, run interactively, a hidden prompt, never a parameter; Enter keeps what this machine saved, else what the old Check-In agent used,
  through the `Get-ArrivalLegacySql` hook of `installScript`), tested on every SMILE database before it is
  saved, enrolls unless `identity.json` exists (re-runs keep the identity; `-Reenroll`
  replaces it; on an enrolled machine the code of another product is attached to the same agent, so
  the same install line works for a second product), runs an optional product setup sub-command (`-Setup`, secrets through env
  `DL_AGENT_SETUP_ARGS`; for `arrival-enroll` the SQL login too, as env `DL_AGENT_ARRIVAL_SQL_AUTH`,
  `DL_AGENT_ARRIVAL_SQL_USER`, `DL_AGENT_SQL_PASSWORD`, never argv; all removed afterwards), only then removes a product's previous agent (`-Replace
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
  the same. Only an open `collect_diagnostics` command of the same agent (handed out, not answered)
  may upload, once (`command_id` required and unique: `400 command_id_required`,
  `409 no_open_diagnostics_command`, `409 diagnostics_already_uploaded`), and at most 4 bundles per
  agent an hour (`429 diagnostics_rate_limited`). Bundles are never served on `/v1/artifacts/*`;
  admins list and download them. Retention: the R2 lifecycle rule `diag-30d` deletes `diag/` objects
  after 30 days (see Production).
- Admin audit: every admin request that changes something (any method but GET/HEAD, after auth) is
  written to `admin_audit` before it runs — time, actor, method, path with query, body with secrets
  replaced by `[redacted]` (JSON fields named like password/pwd/secret/token/authorization/credential,
  `*_sealed` or ending in `key` such as `tenant_key`/`hmac_key`/`api-key`, but not `r2_key`,
  `object_key`, `public_key`, `sort_key`, `cache_key`, `key_id`, `kid`; `Password=`/`Pwd=` in
  connection strings with ADO.NET quoting; `name=value` text pairs with such names; `Bearer`/`Basic`
  values; cut at 4 000 characters) — and its status and error code once it finished. The actor is the
  e-mail (or `service-token:<client id>`) of a verified Cloudflare Access token
  (`Cf-Access-Jwt-Assertion`: RS256 by the team's keys, `aud` = `ACCESS_AUD`, `iss` =
  `https://<ACCESS_TEAM_DOMAIN>`, unexpired); without those vars or a valid token it is `admin-token`
  (the `Cf-Access-Authenticated-User-Email` header alone is never trusted). If the row cannot be
  written the change is refused with `503 audit_unavailable`, except the kill switches (release
  pause/resume, tenant hold `POST`/`DELETE`, agent revoke), which go ahead and log the would-be row
  (`admin_audit_write_failed`, `fail_open: true`).
- A cron trigger prunes the nonce replay table every 15 minutes.

## Release channels for customers

Rule and background: [ADR 0004](../../docs/decisions/0004-customers-on-stable.md).

1. Create a customer tenant with `demo=false` (`PUT /admin/tenants/:id`); its agents follow stable.
2. Canary only for our machines and consented pilots: `PUT /admin/agents/:id/channel {"channel":"canary"}`
   and record why. A machine ahead of stable stays pinned, never downgraded.
3. When stable reaches the machine's version, unpin: `PUT /admin/agents/:id/channel {"channel":"stable"}`.
4. Before promoting, read `GET /admin/releases/:product/:version/gate`; promote only when every check passes.
5. After a test round, `POST /admin/agents/:id/revoke` the offline test agents so `GET /admin/agents`
   shows only live machines (`include_revoked=1` lists the rest).

## Demo bench (one command, RT-0261)

`POST /admin/demo-bench` `{tenant_id, site_id, modules?, expires_hours? (1–72, default 24), settings?}`
→ `201` (`Cache-Control: no-store`) `{tenant_id, site_id, channel: "canary", enroll_code_id,
enroll_code, expires_at, install_command, modules: [{module, product, setup, source, secrets:[{env,
required, what}], options:[{env, flag, required, what, value?}], note}], operator_env: ["$env:… = …"]}`.

- The tenant is created as a demo tenant (canary); a live one answers `409 tenant_not_demo`. The code
  is an ordinary one-time enrolment code (only its SHA-256 in D1, shown once here) whose first config
  enables the modules (`modules`, default all nine). No new table, no migration.
- The hub mints no product code: E-Invoice, PMS, Check-In, Membership, Hotel Link, SRBackup and PDA
  codes come from those clouds, so each module answers `source: "operator"` and the variable the
  operator sets on the PC (`DL_AGENT_DEMO_*`); `speaker` answers `none`; `pms-device` takes its API URL (and an optional token, else generated).
  `settings` takes only the non-secret option variables (https URLs, ids) to prefill `operator_env`;
  a module code there is a `400`.
- `install.ps1 -Demo` (or env `DL_AGENT_DEMO=1`) does the rest on the PC: see
  `crates/dl-agent/README.md` "Demo bench". The module table is `src/domain/demo.ts`, rendered into
  `install.ps1` as `$DemoTable` (`src/install/demo.ts`).

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
  Apply migration `0007_diagnostics_audit.sql` (`pnpm db:migrate`) **before** deploying the Worker
  that has the admin audit: until the table exists every admin change except the kill switches
  answers `503 audit_unavailable`. `HUB_URLS` is a `wrangler.jsonc` var, so a changed list goes live
  with that deploy (agents keep only allow-listed hosts, PROTOCOL.md §2.1).
- Audit actor (optional vars, both or neither): `ACCESS_TEAM_DOMAIN` (`<team>.cloudflareaccess.com`)
  and `ACCESS_AUD` (the admin Access application's audience tag), e.g. in `wrangler.jsonc` `vars`.
- Diagnostics retention (run once, by the operator):
  `wrangler r2 bucket lifecycle add agent-hub-artifacts diag-30d diag/ --expire-days 30`
  (check with `wrangler r2 bucket lifecycle list agent-hub-artifacts`; the same rule as JSON is
  `r2-lifecycle.json`, for `wrangler r2 bucket lifecycle set`, which replaces every rule of the bucket).
- Bench needs `BENCH_KEY` (32 random bytes, base64): `openssl rand -base64 32 | wrangler secret put BENCH_KEY`.
  Rotating it makes every existing bundle unreadable (`500 bench_unreadable`); revoke them first.
- Artifacts: `wrangler r2 object put agent-hub-artifacts/<product>/<version>/<target>/<file> --remote`,
  served to agents at `/v1/artifacts/<key>`.

## Guided install page (RT-0292)

Product consoles (Azolo E-Invoice, Check-In, Membership, PMS, PDAOrder, Hotel Link, SRBackup) should not
show the install line, codes and options to the person installing. Every new code comes with `deploy_url`
(`<hub>/deploy#code=XXXX-XXXX`): a console shows **one button, "Triển khai agent", that opens it** (new
tab). The page (Vietnamese, light/dark, phone width) does the rest:

1. open PowerShell as Administrator on the hotel's server;
2. copy the line (one click) and press Enter;
3. wait: the page polls `POST /deploy/status {code}` and shows the host and version once it enrolled
   (or attached, RT-0298), each module's state, then the install self-test (pass / warn / fail) and a
   done banner. Unknown, expired and revoked codes say to mint a new one from the console.

A product whose console already serves its own install script (it carries the product's one-time
token and setup, e.g. E-Invoice `/agent/bootstrap/{token}/dl/{code}`) links
`<hub>/deploy#code=<dl code>&boot=<script url>[&alt=<variant for SQL Server on another machine>]`
(`productDeployUrl` builds it). A product that mints its hub code during the install (Check-In) sends `#boot=…&p=<product>` without `code`: the page shows the steps and sends the installer back to the product console for the result. The page then shows the line that runs that script and still tracks
the dl code. `boot`/`alt` must be `https://` on a host under `pms.id.vn` or `mmsolutions.vn`
(`BOOT_HOST_SUFFIXES`), with at most a plain query (e.g. `?channel=canary`), no port or quotes; any other link shows "Liên kết cài đặt không hợp
lệ" and no command, so a crafted link cannot put a foreign script on the hub's page.
A product whose script is safe under `iex` (turns TLS 1.2 on itself, no `param()` block) adds `&irm=1`
(`productDeployUrl({ irm: true })`): the page then shows `irm <boot> | iex` (or the `alt` URL), and a
folded "Máy cũ? dùng lệnh này" row with its own copy button keeps the long TLS 1.2 + `WebClient` line
for Windows Server 2012 R2/2016 without TLS 1.2 or PowerShell 2.0. Any other `irm` value, or none,
keeps the long line only (RT-0461).

The code stays in the URL fragment (never sent in a request line or logged) and in POST bodies; the
status answers only for the machine that used that code (host, version, modules, self-test counts:
no tenant config, no other agent). The page runs one nonce'd inline script under a strict CSP, cannot
be framed and is served on the primary host only. "Gặp lỗi khi cài?" lists the usual fixes (rights,
proxy, antivirus, re-run is safe, adding a product to an existing agent).

## Ops console (MCP, read-only, RT-0300)

`POST /mcp` speaks MCP over HTTP (JSON-RPC answered as JSON) for the Request Tracking hub's ops
assistant. Its own bearer `MCP_TOKEN` (`wrangler secret put MCP_TOKEN`; without it `/mcp` answers 401)
is not the admin token, and every tool only reads:

| Tool | Reads |
|---|---|
| `hub_test_results {suite?, limit?}` | last published runs (commit, typecheck, counts, failures): suite `agent-hub` (`pnpm test`, default) or `cargo` (clippy as `typecheck`, `cargo test --workspace` counts per test binary) |
| `repo_file {path, ref?}`, `repo_list {path?, ref?}`, `repo_commits {ref?, limit?}` | the public repository `vzeturn/devlibrary`, default `main` |
| `repo_pull {number}`, `repo_commit {sha}` | one pull request (title, state, merged, head/base sha, description) or one commit (message, parents), each with its changed files and a patch excerpt (4000 characters per file; read a whole file with `repo_file` at that sha). When the unauthenticated API is rate-limited (403/429, Worker IPs are shared), `repo_commits` reads github.com's commit Atom feed (last 20) and `repo_pull` / `repo_commit` the `.patch` (commits, files, patch; a pull request counts as merged when main's feed has its `Merge pull request #n`); each answer then says `source` |
| `admin_get {path}` | any `GET /admin/...` route, run in-process with the admin token (never handed out); `/admin/diagnostics/:id/download` and the HTML page are refused |
| `list_agents`, `get_agent`, `agent_selftest`, `list_releases`, `release_gate`, `list_reports` | the same admin reads by name |
| `agent_survey {agent_id}` | the agent's last site survey (`GET /admin/agents/:id/survey`) with a summary first: `login_source`, and per database `auth_scheme`, `tds`, `server_port`, `instance`, `encryption`, `tls`, engine, warnings (RT-0307) |

Publish a run after testing: `AGENT_HUB_ADMIN_TOKEN=… AGENT_HUB_ACCESS_CLIENT_ID=… AGENT_HUB_ACCESS_CLIENT_SECRET=…
TEST_RUNNER=cloud pnpm test:publish` (typecheck + vitest, then `PUT /admin/test-runs/agent-hub`; the hub
keeps 20 per suite; `--dry-run` only prints). Exit 1 when a check failed, 2 when publishing failed.

Rust results (RT-0287): `node scripts/publish-cargo-run.mjs [--ref <sha|branch>] [--windows]` from the
repository root, with the same variables, publishes suite `cargo`. `--ref` tests any commit (a pull
request's head, for example) in a temporary worktree. Running a pull request's tests runs its code, so
the cargo processes get a scrubbed environment (PATH, HOME and the toolchain folders only: no token,
key or SQL password) and, after `cargo fetch --locked`, run `--offline` in a network namespace with
only loopback (`unshare -rn`); without namespaces the script refuses unless `--no-isolation`, and the
run records `isolated: no`. The admin token is used only for the final `PUT`.
