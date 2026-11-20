export interface CommandRow {
  id: string;
  agent_id: string;
  kind: string;
  module: string | null;
  args: string;
  created_at: number;
  expires_at: number;
  delivered_at: number | null;
  result_ok: number | null;
  result_output: string | null;
  finished_at: number | null;
  result_at: number | null;
  /** JSON object of variables for the agent (remote attach); null once delivered. Never shown. */
  secret: string | null;
}

export async function insertCommand(
  db: D1Database,
  c: Pick<CommandRow, 'id' | 'agent_id' | 'kind' | 'module' | 'args' | 'created_at' | 'expires_at'> & { secret?: string | null },
): Promise<void> {
  await db
    .prepare(
      `INSERT INTO commands (id, agent_id, kind, module, args, created_at, expires_at, secret)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)`,
    )
    .bind(c.id, c.agent_id, c.kind, c.module, c.args, c.created_at, c.expires_at, c.secret ?? null)
    .run();
}

/**
 * Marks every pending, unexpired command delivered and returns them (atomic: each is handed out
 * once). A command's secret is returned with it and cleared from the row by the same call, so
 * it lives on the hub only until the agent's next heartbeat. Expired commands lose theirs too.
 */
export async function takePending(db: D1Database, agentId: string, now: number): Promise<CommandRow[]> {
  const [taken] = await db.batch<CommandRow>([
    db
      .prepare(
        `UPDATE commands SET delivered_at = ?2
         WHERE agent_id = ?1 AND delivered_at IS NULL AND expires_at > ?2
         RETURNING *`,
      )
      .bind(agentId, now),
    db
      .prepare(`UPDATE commands SET secret = NULL WHERE agent_id = ?1 AND secret IS NOT NULL AND (delivered_at IS NOT NULL OR expires_at <= ?2)`)
      .bind(agentId, now),
  ]);
  return [...(taken?.results ?? [])].sort((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id));
}

/** Records the single result of a command; false when unknown, not this agent's, or already answered. */
export async function saveResult(
  db: D1Database,
  agentId: string,
  id: string,
  r: { ok: boolean; output: string; finished_at: number },
  now: number,
): Promise<'saved' | 'not_found' | 'duplicate'> {
  const res = await db
    .prepare(
      `UPDATE commands SET result_ok = ?3, result_output = ?4, finished_at = ?5, result_at = ?6
       WHERE id = ?1 AND agent_id = ?2 AND result_at IS NULL`,
    )
    .bind(id, agentId, r.ok ? 1 : 0, r.output, r.finished_at, now)
    .run();
  if (res.meta.changes === 1) return 'saved';
  const exists = await db.prepare('SELECT 1 AS x FROM commands WHERE id = ?1 AND agent_id = ?2').bind(id, agentId).first();
  return exists ? 'duplicate' : 'not_found';
}

/** One command of one agent, or null. */
export async function agentCommand(db: D1Database, agentId: string, id: string): Promise<CommandRow | null> {
  return db.prepare('SELECT * FROM commands WHERE id = ?1 AND agent_id = ?2').bind(id, agentId).first<CommandRow>();
}

/** Clears the secret of every expired command (a command never delivered keeps none past its life). */
export async function clearExpiredSecrets(db: D1Database, now: number): Promise<void> {
  await db.prepare('UPDATE commands SET secret = NULL WHERE secret IS NOT NULL AND expires_at <= ?1').bind(now).run();
}

export async function listCommands(db: D1Database, agentId: string | undefined): Promise<CommandRow[]> {
  const sql = `SELECT * FROM commands WHERE (?1 IS NULL OR agent_id = ?1) ORDER BY created_at DESC LIMIT 500`;
  return (await db.prepare(sql).bind(agentId ?? null).all<CommandRow>()).results;
}
