# agent-hub

Control plane for `dl-agent`: the hub side of [`crates/dl-agent/PROTOCOL.md`](../../crates/dl-agent/PROTOCOL.md).
A Cloudflare Worker (TypeScript strict, Hono, D1, R2). This is the only non-Rust directory in the repo.

## What it does

| Area | Endpoints |
|---|---|
| Agents (§1–§5) | `POST /v1/enroll` (one-time code), then Ed25519-signed `POST /v1/heartbeat`, `POST /v1/reports`, `POST /v1/commands/:id/result`, `GET /v1/releases/:product/:channel`, `GET /v1/artifacts/:key` (R2 `ARTIFACTS`) |
| Admin (`Authorization: Bearer $ADMIN_TOKEN`) | `PUT /admin/tenants/:id`, `GET /admin/tenants`; `POST/GET /admin/enroll-codes`, `DELETE /admin/enroll-codes/:id`; `GET /admin/agents`, `PUT /admin/agents/:id/channel`, `POST /admin/agents/:id/revoke`, `PUT /admin/agents/:id/config`; `POST/GET /admin/commands`; `GET /admin/releases`, `PUT /admin/releases/:product/:version` (canary manifest as the raw body), `GET …/gate`, `POST …/promote`; `GET /admin/reports?tenant_id&severity&fingerprint&version` |
| Status | `GET /admin` — read-only HTML (Vietnamese) of agents and canary releases with gate status |

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
