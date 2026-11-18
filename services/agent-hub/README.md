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

## Deploy (not done yet)

1. `wrangler d1 create agent-hub-db` and `wrangler r2 bucket create agent-hub-artifacts`.
2. Put `account_id`, the D1 `database_id` and the route in `wrangler.jsonc` (placeholders today).
3. `wrangler secret put ADMIN_TOKEN`, then `pnpm db:migrate` and `pnpm deploy`.
4. Put a Cloudflare Access application on `<hub host>/admin*` (owner e-mail only): browsers cannot send
   the Bearer token, and Access adds a second factor in front of it. `/v1/*` stays public (signed).

The hub domain and the production deploy need the owner's approval (RT-0164).
