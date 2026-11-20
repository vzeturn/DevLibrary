/** Which agent serves a product's tenant (migration 0008, RT-0298). */
export interface ProductLinkRow {
  product: string;
  external_id: string;
  tenant_id: string;
  agent_id: string;
  site_id: string | null;
  created_at: number;
}

export interface ProductLinkFilter {
  tenantId?: string | undefined;
  agentId?: string | undefined;
}

export async function listProductLinks(db: D1Database, f: ProductLinkFilter = {}): Promise<ProductLinkRow[]> {
  const sql = `SELECT * FROM product_links
               WHERE (?1 IS NULL OR tenant_id = ?1) AND (?2 IS NULL OR agent_id = ?2)
               ORDER BY agent_id, product, external_id`;
  return (await db.prepare(sql).bind(f.tenantId ?? null, f.agentId ?? null).all<ProductLinkRow>()).results;
}

export interface AttachWrite {
  codeId: string;
  agentId: string;
  /** Claim the unused code for the agent (false: the agent already holds it). */
  claim: boolean;
  /** Config version read before the merge; the write only lands when it is still current. */
  expectedVersion: number;
  /** The merged config body (JSON), or null when the merge changed nothing. */
  body: string | null;
  link: Omit<ProductLinkRow, 'agent_id' | 'created_at'> | null;
  now: number;
}

/** `ok` false: the code or the config moved meanwhile (read again and retry). */
export interface AttachWritten {
  ok: boolean;
  /** The new config version when a body was written. */
  version: number | null;
}

/**
 * Claims the code (when `claim`), writes the merged config and records the product link in one
 * transaction. The claim only succeeds while the code is unused and the agent's config is still
 * at `expectedVersion`, so two attaches with one code cannot both win and a config changed in
 * between is never overwritten; the config write itself is guarded by the version too. The
 * config and link statements run only while the agent holds the code.
 */
export async function writeAttach(db: D1Database, w: AttachWrite): Promise<AttachWritten> {
  const stmts: D1PreparedStatement[] = [];
  if (w.claim) {
    stmts.push(
      db
        .prepare(
          `UPDATE enroll_codes SET used_at = ?1, used_by_agent = ?2
           WHERE id = ?3 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?1
             AND COALESCE((SELECT version FROM configs WHERE agent_id = ?2), 0) = ?4`,
        )
        .bind(w.now, w.agentId, w.codeId, w.expectedVersion),
    );
  }
  const configAt = w.body === null ? -1 : stmts.length;
  if (w.body !== null) {
    stmts.push(
      db
        .prepare(
          `INSERT INTO configs (agent_id, version, body, updated_at)
           SELECT ?1, 1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM enroll_codes WHERE id = ?4 AND used_by_agent = ?1)
           ON CONFLICT (agent_id) DO UPDATE SET version = configs.version + 1, body = excluded.body,
             updated_at = excluded.updated_at
             WHERE configs.version = ?5
           RETURNING version`,
        )
        .bind(w.agentId, w.body, w.now, w.codeId, w.expectedVersion),
    );
  }
  if (w.link) {
    stmts.push(
      db
        .prepare(
          `INSERT INTO product_links (product, external_id, tenant_id, agent_id, site_id, created_at)
           SELECT ?1, ?2, ?3, ?4, ?5, ?6 WHERE EXISTS (SELECT 1 FROM enroll_codes WHERE id = ?7 AND used_by_agent = ?4)
           ON CONFLICT (product, external_id) DO UPDATE SET tenant_id = excluded.tenant_id,
             site_id = excluded.site_id, agent_id = excluded.agent_id,
             created_at = CASE WHEN product_links.agent_id = excluded.agent_id
                               THEN product_links.created_at ELSE excluded.created_at END`,
        )
        .bind(w.link.product, w.link.external_id, w.link.tenant_id, w.agentId, w.link.site_id, w.now, w.codeId),
    );
  }
  if (stmts.length === 0) return { ok: true, version: null };
  const results = await db.batch<{ version: number }>(stmts);
  const version = configAt >= 0 ? (results[configAt]?.results[0]?.version ?? null) : null;
  if (w.claim) return { ok: (results[0]?.meta.changes ?? 0) === 1, version };
  return { ok: w.body === null || version !== null, version };
}
