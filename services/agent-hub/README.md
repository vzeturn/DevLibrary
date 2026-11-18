# agent-hub

Control plane for `dl-agent`: the hub side of [`crates/dl-agent/PROTOCOL.md`](../../crates/dl-agent/PROTOCOL.md).
A Cloudflare Worker (TypeScript strict, Hono, D1, R2). This is the only non-Rust directory in the repo.

## What it does

| Area | Endpoints |
|---|---|
| Agents (§1–§5) | `POST /v1/enroll` (one-time code), then Ed25519-signed `POST /v1/heartbeat`, `POST /v1/reports`, `POST /v1/commands/:id/result`, `GET /v1/releases/:product/:channel`, `GET /v1/artifacts/:key` (R2 `ARTIFACTS`) |
| Admin (`Authorization: Bearer $ADMIN_TOKEN`) | `PUT /admin/tenants/:id`, `GET /admin/tenants`; `POST/GET /admin/enroll-codes`, `DELETE /admin/enroll-codes/:id`; `GET /admin/agents`, `PUT /admin/agents/:id/channel`, `POST /admin/agents/:id/revoke`, `PUT /admin/agents/:id/config`; `POST/GET /admin/commands`; `GET /admin/releases`, `PUT /admin/releases/:product/:version` (canary manifest as the raw body), `GET …/gate`, `POST …/promote`; `GET /admin/reports?tenant_id&severity&fingerprint&version`; `GET /admin/agents/:id/survey`; `PUT/GET/DELETE /admin/products/:product/site-requirements`, `GET /admin/products/:product/sites?tenant_id` (ready / not ready per site, with reasons); `POST/GET /admin/product-keys`, `DELETE /admin/product-keys/:id` |
| Product platforms (`Authorization: Bearer <product key>`) | `POST /products/:product/enroll-codes` `{external_id, name, site_id?, ttl_seconds?, demo?}` → code for tenant `<product>-<external_id>` (created on first use), agent starts with `modules.<product>.enabled` |
| Install (public, `no-store`) | `GET /install.ps1` (Windows installer, this hub as default), `GET /install/info/:product/:channel/:target` (`key=value` lines: version, sha256, size, `server_time`, download path), `GET /install/download/:product/:channel/:version/:target` (streams the R2 build of a published release, or redirects to an off-hub URL) |
| Status | `GET /admin` — read-only HTML (Vietnamese) of agents, canary releases with gate status, and site readiness per product |

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
- A cron trigger prunes the nonce replay table every 15 minutes.

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
- Artifacts: `wrangler r2 object put agent-hub-artifacts/<product>/<version>/<target>/<file> --remote`,
  served to agents at `/v1/artifacts/<key>`.
