export interface ConfigRow {
  agent_id: string;
  version: number;
  body: string;
  updated_at: number;
}

export async function getConfig(db: D1Database, agentId: string): Promise<ConfigRow | null> {
  return db.prepare('SELECT * FROM configs WHERE agent_id = ?1').bind(agentId).first<ConfigRow>();
}

/** Stores a new body and bumps the version by one; returns the new version. */
export async function putConfig(db: D1Database, agentId: string, body: string, now: number): Promise<number> {
  const version = await db
    .prepare(
      `INSERT INTO configs (agent_id, version, body, updated_at) VALUES (?1, 1, ?2, ?3)
       ON CONFLICT (agent_id) DO UPDATE SET version = configs.version + 1, body = excluded.body,
         updated_at = excluded.updated_at
       RETURNING version`,
    )
    .bind(agentId, body, now)
    .first<number>('version');
  if (version === null) throw new Error('config upsert returned no version');
  return version;
}
