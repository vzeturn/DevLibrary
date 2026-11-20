export interface ProductLinkRow {
  agent_id: string;
  product: string;
  product_tenant: string;
  site_id: string;
  code_id: string;
  linked_at: number;
}

/**
 * The statement recording the link of a product-minted code once `agentId` claimed it at `now`
 * (a no-op for an admin code); runs inside the claim batch so it only lands with the claim.
 */
export function linkFromClaimedCode(db: D1Database, codeId: string, agentId: string, now: number): D1PreparedStatement {
  return db
    .prepare(
      `INSERT INTO product_links (agent_id, product, product_tenant, site_id, code_id, linked_at)
       SELECT used_by_agent, minted_by, tenant_id, site_id, id, used_at FROM enroll_codes
       WHERE id = ?1 AND used_by_agent = ?2 AND used_at = ?3 AND minted_by IS NOT NULL
       ON CONFLICT (agent_id, product) DO UPDATE SET product_tenant = excluded.product_tenant,
         site_id = excluded.site_id, code_id = excluded.code_id, linked_at = excluded.linked_at`,
    )
    .bind(codeId, agentId, now);
}

export interface LinkFilter {
  agent_id?: string;
  product?: string;
  /** Matches the product's tenant or the agent's own tenant. */
  tenant_id?: string;
}

export type ProductLinkView = ProductLinkRow & { agent_tenant: string; agent_site: string; hostname: string };

export async function listLinks(db: D1Database, f: LinkFilter): Promise<ProductLinkView[]> {
  const sql = `SELECT l.*, a.tenant_id AS agent_tenant, a.site_id AS agent_site, a.hostname
               FROM product_links l JOIN agents a ON a.id = l.agent_id
               WHERE (?1 IS NULL OR l.agent_id = ?1) AND (?2 IS NULL OR l.product = ?2)
                 AND (?3 IS NULL OR l.product_tenant = ?3 OR a.tenant_id = ?3)
               ORDER BY l.linked_at DESC, l.product LIMIT 500`;
  return (await db.prepare(sql).bind(f.agent_id ?? null, f.product ?? null, f.tenant_id ?? null).all<ProductLinkView>()).results;
}
