export interface AuditRow {
  id: number;
  at: number;
  actor: string;
  method: string;
  path: string;
  body: string | null;
  status: number | null;
  error: string | null;
}

/** Writes the entry before the change runs; returns its id. */
export async function insertAudit(db: D1Database, row: Omit<AuditRow, 'id' | 'status' | 'error'>): Promise<number> {
  const result = await db
    .prepare('INSERT INTO admin_audit (at, actor, method, path, body) VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id')
    .bind(row.at, row.actor, row.method, row.path, row.body)
    .first<{ id: number }>();
  if (!result) throw new Error('admin_audit insert returned no id');
  return result.id;
}

export async function finishAudit(db: D1Database, id: number, status: number, error: string | null): Promise<void> {
  await db.prepare('UPDATE admin_audit SET status = ?2, error = ?3 WHERE id = ?1').bind(id, status, error).run();
}

export interface AuditFilter {
  since?: number | undefined;
  actor?: string | undefined;
  path_prefix?: string | undefined;
  limit: number;
}

/** Newest first. */
export async function listAudit(db: D1Database, f: AuditFilter): Promise<AuditRow[]> {
  const { results } = await db
    .prepare(
      `SELECT * FROM admin_audit
       WHERE (?1 IS NULL OR at >= ?1) AND (?2 IS NULL OR actor = ?2) AND (?3 IS NULL OR substr(path, 1, length(?3)) = ?3)
       ORDER BY id DESC LIMIT ?4`,
    )
    .bind(f.since ?? null, f.actor ?? null, f.path_prefix ?? null, f.limit)
    .all<AuditRow>();
  return results;
}
