export interface BenchRow {
  id: string;
  tenant_id: string;
  site_id: string;
  label: string;
  enroll_code_id: string;
  steps_enc: string;
  created_at: number;
  expires_at: number;
  fetch_count: number;
  max_fetches: number;
  revoked_at: number | null;
}

export type BenchListRow = Omit<BenchRow, 'steps_enc'> & { code_used_at: number | null };

export async function insertBench(db: D1Database, row: Omit<BenchRow, 'fetch_count' | 'revoked_at'>): Promise<void> {
  await db
    .prepare(
      `INSERT INTO bench_bundles (id, tenant_id, site_id, label, enroll_code_id, steps_enc, created_at, expires_at, max_fetches)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)`,
    )
    .bind(row.id, row.tenant_id, row.site_id, row.label, row.enroll_code_id, row.steps_enc, row.created_at, row.expires_at, row.max_fetches)
    .run();
}

/** Counts one fetch and returns the bundle, atomically; null when unknown, expired, revoked or used up. */
export async function takeFetch(db: D1Database, id: string, now: number): Promise<BenchRow | null> {
  const res = await db
    .prepare(
      `UPDATE bench_bundles SET fetch_count = fetch_count + 1
       WHERE id = ?1 AND revoked_at IS NULL AND expires_at > ?2 AND fetch_count < max_fetches
       RETURNING *`,
    )
    .bind(id, now)
    .all<BenchRow>();
  return res.results[0] ?? null;
}

export async function listBench(db: D1Database): Promise<BenchListRow[]> {
  const sql = `SELECT b.id, b.tenant_id, b.site_id, b.label, b.enroll_code_id, b.created_at, b.expires_at,
                      b.fetch_count, b.max_fetches, b.revoked_at, c.used_at AS code_used_at
               FROM bench_bundles b LEFT JOIN enroll_codes c ON c.id = b.enroll_code_id
               ORDER BY b.created_at DESC LIMIT 200`;
  return (await db.prepare(sql).all<BenchListRow>()).results;
}

/** Revokes the bundle and its enrolment code when still unused, together; false when unknown or already revoked. */
export async function revokeBench(db: D1Database, id: string, now: number): Promise<boolean> {
  const [bundle] = await db.batch([
    db.prepare('UPDATE bench_bundles SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL').bind(id, now),
    db
      .prepare(
        `UPDATE enroll_codes SET revoked_at = ?2
         WHERE id = (SELECT enroll_code_id FROM bench_bundles WHERE id = ?1 AND revoked_at = ?2)
           AND revoked_at IS NULL AND used_at IS NULL`,
      )
      .bind(id, now),
  ]);
  return (bundle?.meta.changes ?? 0) === 1;
}
