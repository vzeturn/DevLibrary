import type { Bump } from '../domain/lanes';

/** Site id that covers every site of a tenant in `release_allows`. */
export const ALL_SITES = '*';

export interface PromotionLane {
  /** NULL on promotions made before migration 0005. */
  bump: Bump | null;
  promoted_at: number;
}

export interface AllowRow {
  tenant_id: string;
  site_id: string;
  created_at: number;
}

/** The latest promotion of a release, or null when it was never promoted. */
export async function latestPromotion(db: D1Database, product: string, version: string): Promise<PromotionLane | null> {
  return db
    .prepare('SELECT bump, promoted_at FROM promotions WHERE product = ?1 AND version = ?2 ORDER BY promoted_at DESC LIMIT 1')
    .bind(product, version)
    .first<PromotionLane>();
}

/** The newest stable release other than `version`: what a promotion of `version` is measured against. */
export async function currentStable(db: D1Database, product: string, version: string): Promise<string | null> {
  return db
    .prepare(`SELECT version FROM releases WHERE product = ?1 AND channel = 'stable' AND version <> ?2 ORDER BY published_at DESC LIMIT 1`)
    .bind(product, version)
    .first<string>('version');
}

export async function addAllow(db: D1Database, product: string, version: string, tenantId: string, siteId: string, now: number): Promise<void> {
  await db
    .prepare(
      `INSERT INTO release_allows (product, version, tenant_id, site_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
       ON CONFLICT (product, version, tenant_id, site_id) DO NOTHING`,
    )
    .bind(product, version, tenantId, siteId, now)
    .run();
}

export async function removeAllow(db: D1Database, product: string, version: string, tenantId: string, siteId: string): Promise<boolean> {
  const res = await db
    .prepare('DELETE FROM release_allows WHERE product = ?1 AND version = ?2 AND tenant_id = ?3 AND site_id = ?4')
    .bind(product, version, tenantId, siteId)
    .run();
  return res.meta.changes > 0;
}

export async function listAllows(db: D1Database, product: string, version: string): Promise<AllowRow[]> {
  const res = await db
    .prepare('SELECT tenant_id, site_id, created_at FROM release_allows WHERE product = ?1 AND version = ?2 ORDER BY tenant_id, site_id')
    .bind(product, version)
    .all<AllowRow>();
  return res.results;
}

/** Whether an admin opted this agent's tenant (all sites) or its site in to the release. */
export async function isAllowed(db: D1Database, product: string, version: string, tenantId: string, siteId: string): Promise<boolean> {
  const row = await db
    .prepare(
      `SELECT 1 AS ok FROM release_allows
       WHERE product = ?1 AND version = ?2 AND tenant_id = ?3 AND site_id IN (?4, '${ALL_SITES}') LIMIT 1`,
    )
    .bind(product, version, tenantId, siteId)
    .first<{ ok: number }>();
  return row !== null;
}
