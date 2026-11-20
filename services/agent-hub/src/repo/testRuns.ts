export interface TestRunRow {
  id: number;
  suite: string;
  commit_sha: string;
  ran_at: number;
  received_at: number;
  ok: number;
  body: string;
}

/** Runs kept per suite; older ones are dropped when a new one arrives. */
export const KEEP_TEST_RUNS = 20;

export async function insertTestRun(db: D1Database, row: Omit<TestRunRow, 'id'>): Promise<number> {
  const [inserted] = await db.batch([
    db
      .prepare('INSERT INTO test_runs (suite, commit_sha, ran_at, received_at, ok, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id')
      .bind(row.suite, row.commit_sha, row.ran_at, row.received_at, row.ok, row.body),
    db
      .prepare(
        `DELETE FROM test_runs WHERE suite = ?1 AND id NOT IN
           (SELECT id FROM test_runs WHERE suite = ?1 ORDER BY id DESC LIMIT ?2)`,
      )
      .bind(row.suite, KEEP_TEST_RUNS),
  ]);
  const id = (inserted?.results?.[0] as { id?: number } | undefined)?.id;
  if (typeof id !== 'number') throw new Error('test_runs insert returned no id');
  return id;
}

export async function listTestRuns(db: D1Database, suite: string, limit: number): Promise<TestRunRow[]> {
  return (
    await db.prepare('SELECT * FROM test_runs WHERE suite = ?1 ORDER BY id DESC LIMIT ?2').bind(suite, Math.min(Math.max(limit, 1), KEEP_TEST_RUNS)).all<TestRunRow>()
  ).results;
}
