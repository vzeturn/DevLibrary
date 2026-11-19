-- Diagnostics bundles and the admin audit trail (RT-0164).

-- One uploaded `collect_diagnostics` bundle; the zip itself is in R2 under `r2_key`
-- (diag/<tenant>/<agent>/<ms>.zip), never served on the agent artifact route.
CREATE TABLE diagnostics (
  id            TEXT PRIMARY KEY,
  agent_id      TEXT NOT NULL REFERENCES agents(id),
  tenant_id     TEXT NOT NULL,
  site_id       TEXT NOT NULL,
  r2_key        TEXT NOT NULL UNIQUE,
  size          INTEGER NOT NULL,
  sha256        TEXT NOT NULL,
  command_id    TEXT,
  agent_version TEXT NOT NULL,
  created_at    INTEGER NOT NULL
);
CREATE INDEX diagnostics_by_agent ON diagnostics (agent_id, created_at DESC);
CREATE INDEX diagnostics_by_tenant ON diagnostics (tenant_id, created_at DESC);

-- Every admin request that changes something (any method but GET/HEAD), written before the
-- change runs; `status` is filled in when it finished (NULL: the Worker died mid-way).
-- `body` is the request body with secrets replaced by "[redacted]", cut at 4 000 characters.
CREATE TABLE admin_audit (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  at         INTEGER NOT NULL,
  actor      TEXT NOT NULL,
  method     TEXT NOT NULL,
  path       TEXT NOT NULL,
  body       TEXT,
  status     INTEGER,
  error      TEXT
);
CREATE INDEX admin_audit_by_time ON admin_audit (at DESC);
