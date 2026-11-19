-- Release pause and tenant hold (RT-0164): the operator's stop buttons for updates.

-- A paused release is never offered (heartbeat update_check, manifest). One row per release,
-- kept apart from `releases` so it survives a republish or a later promotion to stable.
CREATE TABLE release_pauses (
  product    TEXT NOT NULL,
  version    TEXT NOT NULL,
  reason     TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (product, version)
);

-- A held tenant (site_id '*') or site gets no update offer and no manifest, for one product
-- or all of them (product '*'). An explicit update_now command still works.
CREATE TABLE tenant_holds (
  tenant_id  TEXT NOT NULL REFERENCES tenants(id),
  site_id    TEXT NOT NULL DEFAULT '*',
  product    TEXT NOT NULL DEFAULT '*',
  reason     TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, site_id, product)
);
