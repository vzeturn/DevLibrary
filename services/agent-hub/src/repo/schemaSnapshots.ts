export interface SnapshotRow {
  id: string;
  agent_id: string;
  tenant_id: string;
  site_id: string;
  db_name: string;
  schema_hash: string;
  server_version: string;
  agent_version: string;
  table_count: number;
  routine_count: number;
  payload_json: string;
  first_seen: number;
  last_seen: number;
}

export type SnapshotSummaryRow = Omit<SnapshotRow, 'payload_json'>;

/** Stores a database snapshot; the same structure again only moves `last_seen`. */
export async function saveSnapshots(db: D1Database, rows: SnapshotRow[]): Promise<void> {
  await db.batch(
    rows.map((r) =>
      db
        .prepare(
          `INSERT INTO schema_snapshots (id, agent_id, tenant_id, site_id, db_name, schema_hash, server_version,
                                         agent_version, table_count, routine_count, payload_json, first_seen, last_seen)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
           ON CONFLICT (agent_id, db_name, schema_hash) DO UPDATE SET
             last_seen = excluded.last_seen, server_version = excluded.server_version,
             agent_version = excluded.agent_version`,
        )
        .bind(r.id, r.agent_id, r.tenant_id, r.site_id, r.db_name, r.schema_hash, r.server_version, r.agent_version, r.table_count, r.routine_count, r.payload_json, r.first_seen),
    ),
  );
}

export async function listSnapshots(db: D1Database, agentId: string | null): Promise<SnapshotSummaryRow[]> {
  const cols = `id, agent_id, tenant_id, site_id, db_name, schema_hash, server_version, agent_version,
                table_count, routine_count, first_seen, last_seen`;
  const stmt = agentId
    ? db.prepare(`SELECT ${cols} FROM schema_snapshots WHERE agent_id = ?1 ORDER BY db_name, last_seen DESC`).bind(agentId)
    : db.prepare(`SELECT ${cols} FROM schema_snapshots ORDER BY agent_id, db_name, last_seen DESC`);
  return (await stmt.all<SnapshotSummaryRow>()).results;
}

export async function getSnapshot(db: D1Database, id: string): Promise<SnapshotRow | null> {
  return db.prepare('SELECT * FROM schema_snapshots WHERE id = ?1').bind(id).first<SnapshotRow>();
}

/** The newest snapshot of each database, for the given agents (all agents when empty). */
export async function latestPerDatabase(db: D1Database, agentIds: string[]): Promise<SnapshotRow[]> {
  const filter = agentIds.length > 0 ? `AND s.agent_id IN (${agentIds.map((_, i) => `?${i + 1}`).join(',')})` : '';
  const sql = `SELECT s.* FROM schema_snapshots s
               WHERE s.id = (SELECT id FROM schema_snapshots WHERE agent_id = s.agent_id AND db_name = s.db_name
                             ORDER BY last_seen DESC, id DESC LIMIT 1) ${filter}
               ORDER BY s.agent_id, s.db_name`;
  return (await db.prepare(sql).bind(...agentIds).all<SnapshotRow>()).results;
}
