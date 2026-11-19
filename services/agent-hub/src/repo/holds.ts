export const ANY = '*';

export interface PauseRow {
  product: string;
  version: string;
  reason: string | null;
  created_at: number;
}

export interface HoldRow {
  tenant_id: string;
  site_id: string;
  product: string;
  reason: string | null;
  created_at: number;
}

export async function addPause(db: D1Database, product: string, version: string, reason: string | null, now: number): Promise<void> {
  await db
    .prepare(
      `INSERT INTO release_pauses (product, version, reason, created_at) VALUES (?1, ?2, ?3, ?4)
       ON CONFLICT (product, version) DO UPDATE SET reason = excluded.reason`,
    )
    .bind(product, version, reason, now)
    .run();
}

export async function removePause(db: D1Database, product: string, version: string): Promise<boolean> {
  const res = await db.prepare('DELETE FROM release_pauses WHERE product = ?1 AND version = ?2').bind(product, version).run();
  return res.meta.changes > 0;
}

export async function getPause(db: D1Database, product: string, version: string): Promise<PauseRow | null> {
  return db.prepare('SELECT * FROM release_pauses WHERE product = ?1 AND version = ?2').bind(product, version).first<PauseRow>();
}

export async function addHold(db: D1Database, h: Omit<HoldRow, 'created_at'>, now: number): Promise<void> {
  await db
    .prepare(
      `INSERT INTO tenant_holds (tenant_id, site_id, product, reason, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
       ON CONFLICT (tenant_id, site_id, product) DO UPDATE SET reason = excluded.reason`,
    )
    .bind(h.tenant_id, h.site_id, h.product, h.reason, now)
    .run();
}

export async function removeHold(db: D1Database, tenantId: string, siteId: string, product: string): Promise<boolean> {
  const res = await db
    .prepare('DELETE FROM tenant_holds WHERE tenant_id = ?1 AND site_id = ?2 AND product = ?3')
    .bind(tenantId, siteId, product)
    .run();
  return res.meta.changes > 0;
}

export async function listHolds(db: D1Database, tenantId?: string): Promise<HoldRow[]> {
  return (
    await db
      .prepare('SELECT * FROM tenant_holds WHERE (?1 IS NULL OR tenant_id = ?1) ORDER BY tenant_id, site_id, product')
      .bind(tenantId ?? null)
      .all<HoldRow>()
  ).results;
}

/** Whether a hold covers this agent's tenant (all sites) or site, for this product or all. */
export async function isHeld(db: D1Database, tenantId: string, siteId: string, product: string): Promise<boolean> {
  const row = await db
    .prepare(
      `SELECT 1 AS ok FROM tenant_holds
       WHERE tenant_id = ?1 AND site_id IN (?2, '${ANY}') AND product IN (?3, '${ANY}') LIMIT 1`,
    )
    .bind(tenantId, siteId, product)
    .first<{ ok: number }>();
  return row !== null;
}
