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
  /** Initial remote config (JSON) for the agent enrolled with this code; null = built-in default. */
  config: string | null;
  /** Product whose key minted the code; null = admin. */
  minted_by: string | null;
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
      `INSERT INTO enroll_codes (id, code_hash, tenant_id, site_id, created_at, expires_at, config, minted_by)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)`,
    )
    .bind(row.id, row.code_hash, row.tenant_id, row.site_id, row.created_at, row.expires_at, row.config, row.minted_by)
    .run();
}

export async function findCodeByHash(db: D1Database, hash: string): Promise<EnrollCodeRow | null> {
  return db.prepare('SELECT * FROM enroll_codes WHERE code_hash = ?1').bind(hash).first<EnrollCodeRow>();
}

export async function findCodeById(db: D1Database, id: string): Promise<EnrollCodeRow | null> {
  return db.prepare('SELECT * FROM enroll_codes WHERE id = ?1').bind(id).first<EnrollCodeRow>();
}

/**
 * Claims the code, creates the agent, seeds its config from the code (when the code carries
 * one) and, for a product-minted code (tenant `<product>-<external_id>`), records the product
 * link, in one transaction; false when the code was taken meanwhile.
 */
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
    db
      .prepare(
        `INSERT INTO configs (agent_id, version, body, updated_at)
         SELECT ?1, 1, config, ?2 FROM enroll_codes WHERE id = ?3 AND used_by_agent = ?1 AND config IS NOT NULL`,
      )
      .bind(agent.id, now, codeId),
    db
      .prepare(
        `INSERT INTO product_links (product, external_id, tenant_id, agent_id, site_id, created_at)
         SELECT minted_by, substr(tenant_id, length(minted_by) + 2), tenant_id, ?1, site_id, ?2
         FROM enroll_codes
         WHERE id = ?3 AND used_by_agent = ?1 AND minted_by IS NOT NULL
           AND substr(tenant_id, 1, length(minted_by) + 1) = minted_by || '-'
           AND length(tenant_id) > length(minted_by) + 1
         ON CONFLICT (product, external_id) DO UPDATE SET tenant_id = excluded.tenant_id,
           agent_id = excluded.agent_id, site_id = excluded.site_id, created_at = excluded.created_at`,
      )
      .bind(agent.id, now, codeId),
  ]);
  return (claim?.meta.changes ?? 0) === 1;
}

export type CodeView = Omit<EnrollCodeRow, 'code_hash' | 'config'>;

export async function listCodes(db: D1Database, tenantId: string | undefined): Promise<CodeView[]> {
  const sql = `SELECT id, tenant_id, site_id, created_at, expires_at, used_at, used_by_agent, revoked_at, minted_by
               FROM enroll_codes WHERE (?1 IS NULL OR tenant_id = ?1) ORDER BY created_at DESC LIMIT 500`;
  return (await db.prepare(sql).bind(tenantId ?? null).all<CodeView>()).results;
}

export async function revokeCode(db: D1Database, id: string, now: number): Promise<boolean> {
  const res = await db
    .prepare('UPDATE enroll_codes SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL AND used_at IS NULL')
    .bind(id, now)
    .run();
  return res.meta.changes === 1;
}
