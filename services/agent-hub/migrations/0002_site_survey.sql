-- Site survey (PROTOCOL §9.5) and per-product site requirements (the `site_eligible` gate check).

-- Last survey each agent sent; heartbeats carry it only when it changed or once a day.
CREATE TABLE agent_surveys (
  agent_id    TEXT PRIMARY KEY REFERENCES agents(id),
  received_at INTEGER NOT NULL,
  body        TEXT NOT NULL
);

-- What a site needs before it runs a product (host, SQL Server engine, grants, schema probe).
CREATE TABLE site_requirements (
  product    TEXT PRIMARY KEY,
  body       TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
