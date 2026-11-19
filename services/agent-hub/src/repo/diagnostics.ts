export interface DiagnosticsRow {
  id: string;
  agent_id: string;
  tenant_id: string;
  site_id: string;
  r2_key: string;
  size: number;
  sha256: string;
  command_id: string;
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

/** Whether a bundle was already stored for `commandId` (each command uploads once). */
export async function diagnosticsForCommand(db: D1Database, commandId: string): Promise<boolean> {
  return (await db.prepare('SELECT 1 AS x FROM diagnostics WHERE command_id = ?1').bind(commandId).first()) !== null;
}

/** Bundles an agent uploaded at or after `since`. */
export async function countDiagnosticsSince(db: D1Database, agentId: string, since: number): Promise<number> {
  const row = await db
    .prepare('SELECT COUNT(*) AS n FROM diagnostics WHERE agent_id = ?1 AND created_at >= ?2')
    .bind(agentId, since)
    .first<{ n: number }>();
  return row?.n ?? 0;
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
