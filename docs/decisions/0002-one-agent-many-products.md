# 0002. One agent, many products: attach instead of a second enrolment

- Status: accepted
- Date: 2026-11-20
- Scope: `services/agent-hub` (`POST /v1/attach`, `product_links`, admin agent view),
  `crates/dl-agent` (`HubClient::attach`, `dl-agent attach`, installer enrol step)
- Request: RT-0298 (first customer: Mũi Né Bay, Check-In RT-0252 + PDAOrder RT-0225)

## Context

The owner's rule: one customer runs one dl-agent, whatever products it buys. Each product
platform mints its own one-time codes (`POST /products/:product/enroll-codes`, tenant
`<product>-<external_id>`, config `{modules: {<product>: {enabled: true}}}`).

Before this decision a machine enrolled for one product could not take a second one:

- The installer on an enrolled machine printed `Already enrolled: keeping this machine's
  identity` and ignored the new code, so the second product's module stayed disabled.
- `-Reenroll` replaced the identity: a new agent under the second product's tenant, the first
  product's config gone, and the first product's platform still pointing at an agent that no
  longer heartbeats.
- Nothing recorded which agent serves which product tenant once the code was spent.

## Decision

**A second product attaches to the agent the machine already has** (PROTOCOL.md §2.3).

- Signed `POST /v1/attach {code}`. The code is checked like an enrolment code. Its `modules`
  are deep-merged into the agent's config (the code's `modules.<x>` keys win, other modules
  and other top-level keys stay) and the version is bumped like an admin change, so the agent
  takes it through the usual config path with last-known-good. The agent keeps its identity,
  tenant and site.
- The claim, the config write and the link are one D1 batch. The claim is conditional on the
  code being unused and the config version unchanged, and the later writes only land while
  this agent holds the code, so two attaches with one code have one winner and a config an
  admin changed meanwhile is re-read, never overwritten.
- A code this agent already used re-merges (idempotent; no new version when nothing changed),
  so running a product's install line again is harmless. A code without `modules` is refused
  (`400 code_has_no_modules`) and stays unused.
- `product_links (product, external_id) → agent_id, tenant_id, site_id` (migration 0008)
  records which agent serves a product tenant, under the agent's own tenant. Enrolments with
  a product-minted code record it too, and the migration backfills it from
  `enroll_codes.minted_by`. A newer enrolment or attach for the same product tenant moves the
  link. `GET /admin/agents` and `GET /admin/agents/:id` list each agent's `products`.
- Product codes still mint normally when a link exists (the attach refreshes it).
- Each attach writes an `admin_audit` row with actor `agent:<id>` (no code in the row).
- The agent: `HubClient::attach`, `dl-agent attach --code` (needs an identity), and the
  installer's enrol step attaches when the machine is enrolled and a code is given without
  `-Reenroll`. A plain code without modules keeps the old behaviour. Any other attach failure
  is reported at the end of the setup (exit 1) after the service is back with the products it
  had, so a bad second code never takes the first product down.

## Consequences

- The same install line works for a first and a second product; support no longer needs
  `-Reenroll` (which lost the first product) for a second product.
- The second product's tenant (`<product>-<external_id>`) is not used by the agent: the agent
  stays in the tenant of its first enrolment. Product platforms find their agent through
  `product_links`, not through the tenant id.
- Any one-time code with modules can switch modules on for an enrolled agent that holds it.
  Codes are short-lived, single-use and minted by the admin or a product key, so this is the
  same trust as enrolling with that code.
- Hubs older than this change answer `404` to an attach; the installer then fails at the end
  with a clear next step instead of silently ignoring the code (it used to exit 0).
- The Windows 7 / PowerShell 2.0 installer path (`legacyScript.ts`) still only says
  `Already enrolled`; such a machine attaches with `dl-agent attach --code` by hand.

## Later work

- Remote attach from the admin console or a product platform (no install line on the site).
- A product key may only write its own `modules.<product>` section (today a product code
  carries only its own module, but the hub does not enforce it on attach).
- Tenant merge for customers already split into one agent per product.
- Detach: remove a product from an agent (disable its module, drop the link).
- The legacy (PowerShell 2.0) installer path attaching like the binary installer.
