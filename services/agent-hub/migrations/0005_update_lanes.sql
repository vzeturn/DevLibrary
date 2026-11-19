-- Update lanes on the stable channel (RT-0164, docs/decisions/0001-update-lanes.md).

-- The semver step from the stable before it; NULL on promotions made before this migration,
-- which keep the old behaviour (offered to every stable agent, like a patch).
ALTER TABLE promotions ADD COLUMN bump TEXT CHECK (bump IN ('major', 'minor', 'patch', 'initial'));

-- Tenants and sites an admin opted in to a major release; nobody else is offered it.
-- site_id '*' covers every site of the tenant.
CREATE TABLE release_allows (
  product    TEXT NOT NULL,
  version    TEXT NOT NULL,
  tenant_id  TEXT NOT NULL REFERENCES tenants(id),
  site_id    TEXT NOT NULL DEFAULT '*',
  created_at INTEGER NOT NULL,
  PRIMARY KEY (product, version, tenant_id, site_id)
);
