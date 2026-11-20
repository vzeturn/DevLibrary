-- Product links (RT-0298): one dl-agent per customer machine serves several products. Each row
-- says that `product` reaches this agent for the product's own tenant (`product_tenant`, the
-- `<product>-<external_id>` tenant its key minted the code for). Written when an agent enrols or
-- attaches with a product-minted code; the agent keeps its own tenant, site and identity.
CREATE TABLE product_links (
  agent_id       TEXT NOT NULL REFERENCES agents(id),
  product        TEXT NOT NULL,
  product_tenant TEXT NOT NULL,
  site_id        TEXT NOT NULL,
  code_id        TEXT NOT NULL,
  linked_at      INTEGER NOT NULL,
  PRIMARY KEY (agent_id, product)
);
CREATE INDEX product_links_tenant ON product_links (product_tenant);
CREATE INDEX product_links_product ON product_links (product, linked_at);

-- Agents enrolled with a product code before this migration.
INSERT OR IGNORE INTO product_links (agent_id, product, product_tenant, site_id, code_id, linked_at)
SELECT used_by_agent, minted_by, tenant_id, site_id, id, used_at
FROM enroll_codes
WHERE minted_by IS NOT NULL AND used_by_agent IS NOT NULL AND used_at IS NOT NULL
  AND used_by_agent IN (SELECT id FROM agents);
