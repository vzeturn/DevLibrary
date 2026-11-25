import type { WatchAgent } from '../domain/watch';

export const ANY_SITE = '*';

export interface SiteCanaryRow {
  tenant_id: string;
  site_id: string;
  created_at: number;
}

export interface WatchRow {
  verdict: 'good' | 'bad';
  reasons: string;
  decided_at: number;
}

export async function addSiteCanary(db: D1Database, tenantId: string, siteId: string, now: number): Promise<void> {
  await db
    .prepare('INSERT INTO site_canaries (tenant_id, site_id, created_at) VALUES (?1, ?2, ?3) ON CONFLICT (tenant_id, site_id) DO NOTHING')
    .bind(tenantId, siteId, now)
    .run();
}

export async function removeSiteCanary(db: D1Database, tenantId: string, siteId: string): Promise<boolean> {
  const res = await db.prepare('DELETE FROM site_canaries WHERE tenant_id = ?1 AND site_id = ?2').bind(tenantId, siteId).run();
  return res.meta.changes > 0;
}

export async function listSiteCanaries(db: D1Database): Promise<SiteCanaryRow[]> {
  return (await db.prepare('SELECT * FROM site_canaries ORDER BY tenant_id, site_id').all<SiteCanaryRow>()).results;
}

/** When the lane started: promotions before it were never watched and are not judged later. */
export async function laneStartedAt(db: D1Database): Promise<number | null> {
  return db.prepare('SELECT MIN(created_at) AS at FROM site_canaries').first<number>('at');
}

export async function isSiteCanary(db: D1Database, tenantId: string, siteId: string): Promise<boolean> {
  const row = await db
    .prepare('SELECT 1 AS hit FROM site_canaries WHERE tenant_id = ?1 AND site_id IN (?2, ?3) LIMIT 1')
    .bind(tenantId, siteId, ANY_SITE)
    .first<{ hit: number }>();
  return row !== null;
}

export async function getWatch(db: D1Database, product: string, version: string, promotedAt: number): Promise<WatchRow | null> {
  return db
    .prepare('SELECT verdict, reasons, decided_at FROM release_watch WHERE product = ?1 AND version = ?2 AND promoted_at = ?3')
    .bind(product, version, promotedAt)
    .first<WatchRow>();
}

/** Stores the verdict once: the first writer wins, so concurrent heartbeats agree. */
export async function putWatch(
  db: D1Database,
  product: string,
  version: string,
  promotedAt: number,
  verdict: 'good' | 'bad',
  reasons: string[],
  now: number,
): Promise<void> {
  await db
    .prepare(
      `INSERT INTO release_watch (product, version, promoted_at, verdict, reasons, decided_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
       ON CONFLICT (product, version, promoted_at) DO NOTHING`,
    )
    .bind(product, version, promotedAt, verdict, JSON.stringify(reasons), now)
    .run();
}

/** Agents of site-canary sites for the product, with the facts the watch judges. */
export async function watchAgents(db: D1Database, product: string, version: string, now: number): Promise<WatchAgent[]> {
  const sites = `FROM agents a JOIN site_canaries s ON s.tenant_id = a.tenant_id AND s.site_id IN (a.site_id, '*')
                 WHERE a.revoked_at IS NULL AND EXISTS (SELECT 1 FROM agent_versions pv WHERE pv.agent_id = a.id AND pv.product = ?1)`;
  const [agents, modules] = await db.batch<Record<string, unknown>>([
    db
      .prepare(
        `SELECT a.id AS agent_id,
                (SELECT h.received_at FROM heartbeats h WHERE h.agent_id = a.id) AS beat,
                (SELECT h.version FROM heartbeats h WHERE h.agent_id = a.id) AS current_version,
                COALESCE((SELECT v.rolled_back FROM agent_versions v WHERE v.agent_id = a.id AND v.product = ?1 AND v.version = ?2), 0) AS rolled_back,
                (SELECT json_extract(t.summary_json, '$.fail') FROM selftest_runs t WHERE t.agent_id = a.id AND t.agent_version = ?2
                   ORDER BY t.received_at DESC, t.id DESC LIMIT 1) AS fails
         ${sites}`,
      )
      .bind(product, version),
    db.prepare(`SELECT m.agent_id, m.name, m.state FROM agent_modules m WHERE m.agent_id IN (SELECT a.id ${sites})`).bind(product),
  ]);
  const byAgent = new Map<string, { name: string; state: string }[]>();
  for (const m of modules?.results ?? []) {
    const id = String(m.agent_id);
    byAgent.set(id, [...(byAgent.get(id) ?? []), { name: String(m.name), state: String(m.state) }]);
  }
  return (agents?.results ?? []).map((a) => ({
    agent_id: String(a.agent_id),
    silent_seconds: a.beat == null ? Number.MAX_SAFE_INTEGER : Math.max(0, now - Number(a.beat)),
    on_version: a.current_version === version,
    rolled_back: Number(a.rolled_back) === 1,
    selftest_failures: Number(a.fails ?? 0),
    modules: byAgent.get(String(a.agent_id)) ?? [],
  }));
}

/** A newer promotion of the product was pulled back: this release is the fallback being served, not under watch. */
export async function newerPulledBack(db: D1Database, product: string, version: string, promotedAt: number): Promise<boolean> {
  const row = await db
    .prepare(`SELECT 1 AS hit FROM release_watch WHERE product = ?1 AND version <> ?2 AND verdict = 'bad' AND promoted_at > ?3 LIMIT 1`)
    .bind(product, version, promotedAt)
    .first<{ hit: number }>();
  return row !== null;
}

/** Forgets the verdicts of a release, so resuming a pulled-back release lets the lane judge it again. */
export async function clearWatch(db: D1Database, product: string, version: string): Promise<void> {
  await db.prepare('DELETE FROM release_watch WHERE product = ?1 AND version = ?2').bind(product, version).run();
}
