export interface DiagnosticsRow {
  id: string;
  agent_id: string;
  tenant_id: string;
  site_id: string;
  r2_key: string;
  size: number;
  sha256: string;
  command_id: string | null;
  agent_version: string;
  created_at: number;
}

export async function insertDiagnostics(db: D1Database, row: DiagnosticsRow): Promise<void> {
  await db
    .prepare(
      `INSERT INTO diagnostics (id, agent_id, tenant_id, site_id, r2_key, size, sha256, command_id, agent_version, created_at)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)`,
    )
    .bind(row.id, row.agent_id, row.tenant_id, row.site_id, row.r2_key, row.size, row.sha256, row.command_id, row.agent_version, row.created_at)
    .run();
}

export interface DiagnosticsFilter {
  agent_id?: string | undefined;
  tenant_id?: string | undefined;
  limit: number;
}

/** Newest first. */
export async function listDiagnostics(db: D1Database, f: DiagnosticsFilter): Promise<DiagnosticsRow[]> {
  const { results } = await db
    .prepare(
      `SELECT * FROM diagnostics
       WHERE (?1 IS NULL OR agent_id = ?1) AND (?2 IS NULL OR tenant_id = ?2)
       ORDER BY created_at DESC, id DESC LIMIT ?3`,
    )
    .bind(f.agent_id ?? null, f.tenant_id ?? null, f.limit)
    .all<DiagnosticsRow>();
  return results;
}

export async function getDiagnostics(db: D1Database, id: string): Promise<DiagnosticsRow | null> {
  return db.prepare('SELECT * FROM diagnostics WHERE id = ?1').bind(id).first<DiagnosticsRow>();
}
