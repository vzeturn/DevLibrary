# 0002. One agent per customer machine: attach products to it

- Status: accepted
- Date: 2026-11-20
- Scope: `services/agent-hub` (`POST /v1/attach`, `product_links`), `crates/dl-agent` (`attach`, install)

## Context

The owner (2026-11-20): a customer may use several or all products (PMS, E-Invoice, Check-In,
Membership, PDAOrder, Hotel Link, SRBackup). dl-agent was built so that one agent covers every
product, so a customer needs one agent. A site usually starts with one product and adds others
later. The first customer, Mui Ne Bay, runs Check-In and PDAOrder on the same machine.

Each product mints dl-agent codes with its own key, for its own tenant `<product>-<external_id>`.
Before this decision a second product's install line on an enrolled machine skipped its code
("Already enrolled"), so the new module stayed off; `--reenroll` gave the machine a new identity
and dropped the first product's config.

## Decision

- A product code given to an enrolled agent is spent by a signed `POST /v1/attach`. The agent
  keeps its id, key, tenant, site and channel. In one D1 batch the hub claims the code for that
  agent, merges the code's `modules` into the agent's config (`json_patch`, RFC 7396; version + 1,
  delivered on the next heartbeat) and upserts `product_links (agent_id, product)` with the
  product's tenant.
- The agent's tenant stays the one it enrolled with; the link table, not the tenant, says which
  product tenants an agent serves. Enrolment with a product code writes the link too.
- An admin code (no product) attaches only inside the agent's own tenant.
- `dl-agent install` with a code on an enrolled machine attaches. A refused attach there is a
  notice, not a failure, because re-running an old line with its spent code must still update.
  `dl-agent attach --code` fails loudly.

## Consequences

- Every product keeps the same one-line installer; the binary decides between enrol and attach.
- A product cannot take over another product's module: it only turns on the modules its code
  carries, and its key only mints codes for its own `<product>-` tenants.
- Agents released before this change still print "Already enrolled" until they update; on those
  machines `dl-agent attach` arrives with the next canary/stable build.
- Removing a product from an agent (turning its module off, dropping the link) is not automated
  yet; the admin edits the config and the link stays as history.
