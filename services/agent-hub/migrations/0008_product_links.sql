-- One customer, one dl-agent (RT-0298, docs/decisions/0002-one-agent-many-products.md): which
-- agent serves a product's tenant. A product mints codes for `<product>-<external_id>`; the
-- agent that enrols or attaches with such a code is recorded here, under the agent's own tenant
-- (an agent enrolled first for another product keeps its tenant).
CREATE TABLE product_links (
  product     TEXT NOT NULL,
  external_id TEXT NOT NULL,
  tenant_id   TEXT NOT NULL,
  agent_id    TEXT NOT NULL REFERENCES agents(id),
  site_id     TEXT,
  created_at  INTEGER NOT NULL,
  PRIMARY KEY (product, external_id)
);
CREATE INDEX product_links_agent ON product_links (agent_id);

-- Backfill: agents enrolled with a product-minted code (`enroll_codes.minted_by`). When several
-- agents used codes of one (product, external_id), the newest live one wins (rows go in oldest
-- first, revoked agents before live ones, and each later row replaces the earlier).
INSERT INTO product_links (product, external_id, tenant_id, agent_id, site_id, created_at)
SELECT c.minted_by, substr(c.tenant_id, length(c.minted_by) + 2), a.tenant_id, a.id, a.site_id, c.used_at
FROM enroll_codes c
JOIN agents a ON a.id = c.used_by_agent
WHERE c.minted_by IS NOT NULL
  AND c.used_at IS NOT NULL
  AND substr(c.tenant_id, 1, length(c.minted_by) + 1) = c.minted_by || '-'
  AND length(c.tenant_id) > length(c.minted_by) + 1
ORDER BY (a.revoked_at IS NULL), c.used_at
ON CONFLICT (product, external_id) DO UPDATE SET
  tenant_id = excluded.tenant_id,
  agent_id = excluded.agent_id,
  site_id = excluded.site_id,
  created_at = excluded.created_at;
