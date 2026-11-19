-- Bench bundles and agent self tests (RT-0164 BENCH).

-- A bench bundle is one `irm https://<hub>/bench/<token> | iex` line for a test PC: install dl-agent on
-- canary for a demo tenant, then run each product's setup step. `id` is hex(sha256(token)); the token
-- is shown once. `steps_enc` holds the enrolment code, steps (their env may carry secrets) and SQL
-- database names, AES-GCM encrypted with the Worker secret BENCH_KEY (the row id is the AAD).
CREATE TABLE bench_bundles (
  id             TEXT PRIMARY KEY,
  tenant_id      TEXT NOT NULL REFERENCES tenants(id),
  site_id        TEXT NOT NULL,
  label          TEXT NOT NULL,
  enroll_code_id TEXT NOT NULL REFERENCES enroll_codes(id),
  steps_enc      TEXT NOT NULL,
  created_at     INTEGER NOT NULL,
  expires_at     INTEGER NOT NULL,
  fetch_count    INTEGER NOT NULL DEFAULT 0,
  max_fetches    INTEGER NOT NULL,
  revoked_at     INTEGER
);
CREATE INDEX bench_bundles_created ON bench_bundles (created_at);

-- Self-test runs an agent uploads (`dl-agent selftest`); the hub keeps the last 20 per agent.
CREATE TABLE selftest_runs (
  id            TEXT PRIMARY KEY,
  agent_id      TEXT NOT NULL REFERENCES agents(id),
  run_id        TEXT NOT NULL,
  trigger       TEXT NOT NULL,
  started_at    INTEGER NOT NULL,
  finished_at   INTEGER NOT NULL,
  agent_version TEXT NOT NULL,
  summary_json  TEXT NOT NULL,                 -- {pass, warn, fail, skip, total, os, hostname}
  checks_json   TEXT NOT NULL,                 -- the checks as sent
  received_at   INTEGER NOT NULL
);
CREATE INDEX selftest_runs_agent ON selftest_runs (agent_id, received_at);
-- An agent retrying an upload sends the same run_id: the run is stored once.
CREATE UNIQUE INDEX selftest_runs_run ON selftest_runs (agent_id, run_id);
