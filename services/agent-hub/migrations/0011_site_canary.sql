-- Site canary lane (RT-1106): sites that receive a stable release first and are watched
-- before it opens to the other sites.

-- A site on the list gets every non-major stable release at once. site_id '*' covers the tenant.
CREATE TABLE site_canaries (
  tenant_id  TEXT NOT NULL REFERENCES tenants(id),
  site_id    TEXT NOT NULL DEFAULT '*',
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, site_id)
);

-- The watch verdict of one promotion, written once: `good` opens the release to the ramp,
-- `bad` pauses it. promoted_at ties the verdict to the promotion it judged (a re-promotion
-- is watched again).
CREATE TABLE release_watch (
  product     TEXT NOT NULL,
  version     TEXT NOT NULL,
  promoted_at INTEGER NOT NULL,
  verdict     TEXT NOT NULL CHECK (verdict IN ('good', 'bad')),
  reasons     TEXT NOT NULL,
  decided_at  INTEGER NOT NULL,
  PRIMARY KEY (product, version, promoted_at)
);
