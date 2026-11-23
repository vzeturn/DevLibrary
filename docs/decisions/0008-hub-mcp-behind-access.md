# 0008. The agent-hub `/mcp` console sits behind a Cloudflare Access service token

- Status: accepted
- Date: 2026-11-23
- Scope: `services/agent-hub` (`/mcp`), Request Tracking RT-0781 (part of RT-0592)

## Context

`https://agent.pms.id.vn/mcp` is the read-only ops console of the hub. It answered any caller on the Internet
that held `MCP_TOKEN`: one leaked Bearer was enough. The Request Tracking hub reaches it through a per-project
transport (RT ADR 0096) and, for consoles that cannot sit behind a Worker binding, adds an Access service token to
every call (RT ADR 0099).

The same Worker also serves `/install.ps1`, `/install/*` and `/v1/*` to hotel PCs. Those callers cannot hold an
Access token (`irm <hub>/install.ps1 | iex` on a fresh machine; the agent has only its signed identity), and the
installer script carries no secret: an enrollment code and the agent's signature gate everything that matters.

## Decision

- `/mcp` requires, in addition to `Authorization: Bearer MCP_TOKEN`, a valid `Cf-Access-Jwt-Assertion` minted by its
  **own** Access application (service-token policy; only the RT hub's token is allowed). The Worker verifies the
  RS256 signature, `iss`, `aud` and `exp` itself (`lib/accessJwt.ts`); the `Cf-Access-*` id headers alone are never
  trusted.
- Switch: `MCP_ACCESS_REQUIRED="true"` plus `ACCESS_TEAM_DOMAIN` and `MCP_ACCESS_AUD` (a different audience from
  `ACCESS_AUD`, which fronts `/admin`). Required but not configured fails closed: 403 `access_required`. Not set:
  behaviour unchanged, so the code ships before the Access application exists.
- A call without a valid token gets 403 `access_required` before the Bearer is looked at; a valid token without the
  Bearer gets 401. Both halves are needed.
- `/install.ps1`, `/install/*`, `/v1/*` and `/healthz` stay public by design (see Context). `/admin` keeps its admin
  token and Access-named audit trail.

## Consequences

- Turning it on: create the Access application for `agent.pms.id.vn/mcp` with a service-token policy, put the service
  token in the RT kit of this project (`OPS_<PROJECT>_CF_ACCESS_CLIENT_ID/SECRET`, RT ADR 0099), set the three
  vars, deploy. Until then the Bearer alone still works.
- Only the RT hub (and the iMac, which pulls the kit) can call `/mcp`; a leaked `MCP_TOKEN` is no longer enough.

## Rollout (2026-11-23)

Access application `DevLibrary agent hub ops MCP (RT-0781)` on `agent.pms.id.vn/mcp`, policy "RT hub service token only"
(the hub's token and one test token). The hub token is in the RT kit of `dev-library` as
`OPS_DEV_LIBRARY_CF_ACCESS_CLIENT_ID/SECRET`; the test token as `CF_ACCESS_TEST_*_DEV_LIBRARY`. The vars are set in
`wrangler.jsonc` and the Worker is deployed. Seen on the live host: no headers 403, wrong Bearer alone 403, service
token without the Bearer 401 (Access and the Worker's JWT check passed, the Bearer did not), `/healthz` and
`/install.ps1` 200. Rotation: create a new service token, write both kit names (merge), then replace the token in the
policy; the old one stops working at once.
