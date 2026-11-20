-- Hub test runs (RT-0300): a session that ran `pnpm test` publishes its summary so the Request
-- Tracking ops assistant can read it through the read-only MCP console (`hub_test_results`).
-- Only counts and failure messages; the hub keeps the last 20 runs per suite.
CREATE TABLE test_runs (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  suite       TEXT NOT NULL,
  commit_sha  TEXT NOT NULL,
  ran_at      INTEGER NOT NULL,
  received_at INTEGER NOT NULL,
  ok          INTEGER NOT NULL,
  body        TEXT NOT NULL
);
CREATE INDEX test_runs_suite ON test_runs (suite, id);
