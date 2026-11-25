-- Schema dictionary (RT-1215, phase 2 of RT-1183): structure-only snapshots of a site's SQL
-- Server databases, kept per (agent, database, schema hash). No customer rows are ever stored.
CREATE TABLE schema_snapshots (
  id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL REFERENCES agents(id),
  tenant_id TEXT NOT NULL,
  site_id TEXT NOT NULL,
  db_name TEXT NOT NULL,
  schema_hash TEXT NOT NULL,
  server_version TEXT NOT NULL,
  agent_version TEXT NOT NULL,
  table_count INTEGER NOT NULL,
  routine_count INTEGER NOT NULL,
  payload_json TEXT NOT NULL,
  first_seen INTEGER NOT NULL,
  last_seen INTEGER NOT NULL
);
-- A re-upload of the same structure only moves last_seen.
CREATE UNIQUE INDEX schema_snapshots_version ON schema_snapshots (agent_id, db_name, schema_hash);
CREATE INDEX schema_snapshots_latest ON schema_snapshots (agent_id, db_name, last_seen);
