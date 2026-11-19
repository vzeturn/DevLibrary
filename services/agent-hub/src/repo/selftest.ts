export interface SelftestRow {
  id: string;
  agent_id: string;
  run_id: string;
  trigger: string;
  started_at: number;
  finished_at: number;
  agent_version: string;
  summary_json: string;
  checks_json: string;
  received_at: number;
}

export type SelftestSummaryRow = Omit<SelftestRow, 'checks_json'>;

/** The latest run of every agent, with where the agent sits (status page). */
export interface LatestSelftestRow extends SelftestRow {
  tenant_id: string;
  site_id: string;
  hostname: string;
}

/** Runs kept per agent. */
export const KEEP_RUNS = 20;

/**
 * Stores a run (a retried upload with the same run_id replaces it) and drops the agent's runs
 * beyond the newest {@link KEEP_RUNS}, in one transaction.
 */
export async function saveSelftest(db: D1Database, row: SelftestRow): Promise<void> {
  await db.batch([
    db
      .prepare(
        `INSERT INTO selftest_runs (id, agent_id, run_id, trigger, started_at, finished_at, agent_version,
                                    summary_json, checks_json, received_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (agent_id, run_id) DO UPDATE SET
           trigger = excluded.trigger, started_at = excluded.started_at, finished_at = excluded.finished_at,
           agent_version = excluded.agent_version, summary_json = excluded.summary_json,
           checks_json = excluded.checks_json, received_at = excluded.received_at`,
      )
      .bind(
        row.id,
        row.agent_id,
        row.run_id,
        row.trigger,
        row.started_at,
        row.finished_at,
        row.agent_version,
        row.summary_json,
        row.checks_json,
        row.received_at,
      ),
    db
      .prepare(
        `DELETE FROM selftest_runs WHERE agent_id = ?1 AND id NOT IN (
           SELECT id FROM selftest_runs WHERE agent_id = ?1 ORDER BY received_at DESC, id DESC LIMIT ?2)`,
      )
      .bind(row.agent_id, KEEP_RUNS),
  ]);
}

export async function latestSelftest(db: D1Database, agentId: string): Promise<SelftestRow | null> {
  return db
    .prepare('SELECT * FROM selftest_runs WHERE agent_id = ?1 ORDER BY received_at DESC, id DESC LIMIT 1')
    .bind(agentId)
    .first<SelftestRow>();
}

export async function selftestHistory(db: D1Database, agentId: string): Promise<SelftestSummaryRow[]> {
  const sql = `SELECT id, agent_id, run_id, trigger, started_at, finished_at, agent_version, summary_json, received_at
               FROM selftest_runs WHERE agent_id = ?1 ORDER BY received_at DESC, id DESC`;
  return (await db.prepare(sql).bind(agentId).all<SelftestSummaryRow>()).results;
}

export async function latestSelftestPerAgent(db: D1Database): Promise<LatestSelftestRow[]> {
  const sql = `SELECT s.*, a.tenant_id, a.site_id, a.hostname
               FROM selftest_runs s JOIN agents a ON a.id = s.agent_id
               WHERE s.id = (SELECT id FROM selftest_runs WHERE agent_id = s.agent_id ORDER BY received_at DESC, id DESC LIMIT 1)
               ORDER BY a.tenant_id, a.site_id, a.hostname`;
  return (await db.prepare(sql).all<LatestSelftestRow>()).results;
}
