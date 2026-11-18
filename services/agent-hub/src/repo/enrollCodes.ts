export interface EnrollCodeRow {
  id: string;
  code_hash: string;
  tenant_id: string;
  site_id: string;
  created_at: number;
  expires_at: number;
  used_at: number | null;
  used_by_agent: string | null;
  revoked_at: number | null;
}

export interface NewAgent {
  id: string;
  public_key: string;
  hostname: string;
  os: string;
  arch: string;
  version: string;
  modules: string[];
  heartbeat_seconds: number;
}

export async function insertCode(db: D1Database, row: Omit<EnrollCodeRow, 'used_at' | 'used_by_agent' | 'revoked_at'>): Promise<void> {
  await db
    .prepare(
      `INSERT INTO enroll_codes (id, code_hash, tenant_id, site_id, created_at, expires_at)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6)`,
    )
    .bind(row.id, row.code_hash, row.tenant_id, row.site_id, row.created_at, row.expires_at)
    .run();
}

export async function findCodeByHash(db: D1Database, hash: string): Promise<EnrollCodeRow | null> {
  return db.prepare('SELECT * FROM enroll_codes WHERE code_hash = ?1').bind(hash).first<EnrollCodeRow>();
}

/** Claims the code and creates the agent in one transaction; false when the code was taken meanwhile. */
export async function claimCodeAndCreateAgent(db: D1Database, codeId: string, agent: NewAgent, now: number): Promise<boolean> {
  const [claim] = await db.batch([
    db
      .prepare(
        `UPDATE enroll_codes SET used_at = ?1, used_by_agent = ?2
         WHERE id = ?3 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?1`,
      )
      .bind(now, agent.id, codeId),
    db
      .prepare(
        `INSERT INTO agents (id, tenant_id, site_id, public_key, hostname, os, arch, version, modules,
                             heartbeat_seconds, enrolled_at)
         SELECT ?1, tenant_id, site_id, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
         FROM enroll_codes WHERE id = ?10 AND used_by_agent = ?1`,
      )
      .bind(
        agent.id,
        agent.public_key,
        agent.hostname,
        agent.os,
        agent.arch,
        agent.version,
        JSON.stringify(agent.modules),
        agent.heartbeat_seconds,
        now,
        codeId,
      ),
  ]);
  return (claim?.meta.changes ?? 0) === 1;
}

export async function listCodes(db: D1Database, tenantId: string | undefined): Promise<Omit<EnrollCodeRow, 'code_hash'>[]> {
  const sql = `SELECT id, tenant_id, site_id, created_at, expires_at, used_at, used_by_agent, revoked_at
               FROM enroll_codes WHERE (?1 IS NULL OR tenant_id = ?1) ORDER BY created_at DESC LIMIT 500`;
  return (await db.prepare(sql).bind(tenantId ?? null).all<Omit<EnrollCodeRow, 'code_hash'>>()).results;
}

export async function revokeCode(db: D1Database, id: string, now: number): Promise<boolean> {
  const res = await db
    .prepare('UPDATE enroll_codes SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL AND used_at IS NULL')
    .bind(id, now)
    .run();
  return res.meta.changes === 1;
}
