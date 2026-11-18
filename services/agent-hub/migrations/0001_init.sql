-- agent-hub schema v1 (see crates/dl-agent/PROTOCOL.md). Times are Unix seconds.

CREATE TABLE tenants (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  demo       INTEGER NOT NULL DEFAULT 0 CHECK (demo IN (0, 1)),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE sites (
  tenant_id  TEXT NOT NULL REFERENCES tenants(id),
  id         TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, id)
);

CREATE TABLE enroll_codes (
  id            TEXT PRIMARY KEY,
  code_hash     TEXT NOT NULL UNIQUE,          -- hex(sha256(code)); the plain code is never stored
  tenant_id     TEXT NOT NULL REFERENCES tenants(id),
  site_id       TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER NOT NULL,
  used_at       INTEGER,
  used_by_agent TEXT,
  revoked_at    INTEGER
);
CREATE INDEX enroll_codes_tenant ON enroll_codes (tenant_id, created_at);

CREATE TABLE agents (
  id                TEXT PRIMARY KEY,
  tenant_id         TEXT NOT NULL REFERENCES tenants(id),
  site_id           TEXT NOT NULL,
  public_key        TEXT NOT NULL,             -- base64 raw Ed25519 public key
  hostname          TEXT NOT NULL,
  os                TEXT NOT NULL,
  arch              TEXT NOT NULL,
  version           TEXT NOT NULL,
  modules           TEXT NOT NULL DEFAULT '[]',
  channel_override  TEXT CHECK (channel_override IN ('canary', 'stable')),
  heartbeat_seconds INTEGER NOT NULL,
  enrolled_at       INTEGER NOT NULL,
  revoked_at        INTEGER,
  last_seen_at      INTEGER
);
CREATE INDEX agents_tenant ON agents (tenant_id, site_id);

CREATE TABLE nonces (
  agent_id TEXT NOT NULL,
  nonce    TEXT NOT NULL,
  seen_at  INTEGER NOT NULL,
  PRIMARY KEY (agent_id, nonce)
);
CREATE INDEX nonces_seen ON nonces (seen_at);

-- Latest heartbeat per agent (raw JSON kept for the admin view).
CREATE TABLE heartbeats (
  agent_id    TEXT PRIMARY KEY REFERENCES agents(id),
  received_at INTEGER NOT NULL,
  version     TEXT NOT NULL,
  channel     TEXT NOT NULL,
  body        TEXT NOT NULL
);

CREATE TABLE agent_modules (
  agent_id   TEXT NOT NULL REFERENCES agents(id),
  name       TEXT NOT NULL,
  state      TEXT NOT NULL,
  detail     TEXT,
  restarts   INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (agent_id, name)
);

-- Which versions each agent has run, and when; the canary gate reads this.
CREATE TABLE agent_versions (
  agent_id    TEXT NOT NULL REFERENCES agents(id),
  product     TEXT NOT NULL,
  version     TEXT NOT NULL,
  first_seen  INTEGER NOT NULL,
  last_seen   INTEGER NOT NULL,
  rolled_back INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (agent_id, product, version)
);
CREATE INDEX agent_versions_gate ON agent_versions (product, version);

CREATE TABLE reports (
  agent_id         TEXT NOT NULL REFERENCES agents(id),
  fingerprint      TEXT NOT NULL,
  tenant_id        TEXT NOT NULL,
  severity         TEXT NOT NULL CHECK (severity IN ('info', 'warning', 'error', 'critical')),
  module           TEXT NOT NULL,
  code             TEXT NOT NULL,
  message          TEXT NOT NULL,
  context          TEXT NOT NULL DEFAULT '{}',
  first_seen       INTEGER NOT NULL,
  last_seen        INTEGER NOT NULL,
  count            INTEGER NOT NULL,
  agent_version    TEXT NOT NULL,
  os               TEXT,
  hostname         TEXT,
  received_first_at INTEGER NOT NULL,
  received_last_at INTEGER NOT NULL,
  PRIMARY KEY (agent_id, fingerprint)
);
CREATE INDEX reports_tenant ON reports (tenant_id, last_seen);
CREATE INDEX reports_fingerprint ON reports (fingerprint);
CREATE INDEX reports_version ON reports (agent_version, severity);

-- Every (fingerprint, version) pair ever reported: "new fingerprint vs previous version".
CREATE TABLE report_versions (
  fingerprint       TEXT NOT NULL,
  version           TEXT NOT NULL,
  severity          TEXT NOT NULL,
  first_received_at INTEGER NOT NULL,
  PRIMARY KEY (fingerprint, version)
);
CREATE INDEX report_versions_version ON report_versions (version, severity);

CREATE TABLE commands (
  id            TEXT PRIMARY KEY,
  agent_id      TEXT NOT NULL REFERENCES agents(id),
  kind          TEXT NOT NULL,
  module        TEXT,
  args          TEXT NOT NULL DEFAULT '{}',
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER NOT NULL,
  delivered_at  INTEGER,
  result_ok     INTEGER,
  result_output TEXT,
  finished_at   INTEGER,
  result_at     INTEGER
);
CREATE INDEX commands_pending ON commands (agent_id, delivered_at, expires_at);

CREATE TABLE configs (
  agent_id   TEXT PRIMARY KEY REFERENCES agents(id),
  version    INTEGER NOT NULL,
  body       TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);

-- Signed manifests stored verbatim: the hub never re-serialises them.
CREATE TABLE releases (
  product      TEXT NOT NULL,
  version      TEXT NOT NULL,
  channel      TEXT NOT NULL CHECK (channel IN ('canary', 'stable')),
  manifest     TEXT NOT NULL,
  published_at INTEGER NOT NULL,
  created_at   INTEGER NOT NULL,
  PRIMARY KEY (product, version, channel)
);
CREATE INDEX releases_latest ON releases (product, channel, published_at);

CREATE TABLE promotions (
  id          TEXT PRIMARY KEY,
  product     TEXT NOT NULL,
  version     TEXT NOT NULL,
  forced      INTEGER NOT NULL CHECK (forced IN (0, 1)),
  reason      TEXT,
  checks      TEXT NOT NULL,                   -- gate result at promotion time (JSON)
  promoted_at INTEGER NOT NULL
);
CREATE INDEX promotions_release ON promotions (product, version);
