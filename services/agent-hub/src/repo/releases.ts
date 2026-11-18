import type { Channel } from '../env';
import type { GateAgent } from '../domain/gate';

export interface ReleaseRow {
  product: string;
  version: string;
  channel: Channel;
  manifest: string;
  published_at: number;
  created_at: number;
}

export async function putRelease(db: D1Database, r: ReleaseRow): Promise<void> {
  await db
    .prepare(
      `INSERT INTO releases (product, version, channel, manifest, published_at, created_at)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6)
       ON CONFLICT (product, version, channel) DO UPDATE SET manifest = excluded.manifest,
         published_at = excluded.published_at, created_at = excluded.created_at`,
    )
    .bind(r.product, r.version, r.channel, r.manifest, r.published_at, r.created_at)
    .run();
}

export async function getRelease(db: D1Database, product: string, version: string, channel: Channel): Promise<ReleaseRow | null> {
  return db
    .prepare('SELECT * FROM releases WHERE product = ?1 AND version = ?2 AND channel = ?3')
    .bind(product, version, channel)
    .first<ReleaseRow>();
}

export async function latestRelease(db: D1Database, product: string, channel: Channel): Promise<ReleaseRow | null> {
  return db
    .prepare('SELECT * FROM releases WHERE product = ?1 AND channel = ?2 ORDER BY published_at DESC LIMIT 1')
    .bind(product, channel)
    .first<ReleaseRow>();
}

export async function listReleases(db: D1Database): Promise<Omit<ReleaseRow, 'manifest'>[]> {
  const res = await db
    .prepare(
      `SELECT product, version, channel, published_at, created_at FROM releases
       ORDER BY product, published_at DESC, channel LIMIT 200`,
    )
    .all<Omit<ReleaseRow, 'manifest'>>();
  return res.results;
}

/** The release before this one: the latest stable published earlier, else the latest earlier canary. */
export async function previousVersion(db: D1Database, product: string, version: string, publishedAt: number): Promise<string | null> {
  return db
    .prepare(
      `SELECT version FROM releases WHERE product = ?1 AND version <> ?2 AND published_at < ?3
       ORDER BY (channel = 'stable') DESC, published_at DESC LIMIT 1`,
    )
    .bind(product, version, publishedAt)
    .first<string>('version');
}

const CANARY_AGENTS = `
  FROM agent_versions v
  JOIN agents a ON a.id = v.agent_id
  JOIN tenants t ON t.id = a.tenant_id
  WHERE v.product = ?1 AND v.version = ?2 AND a.revoked_at IS NULL
    AND COALESCE(a.channel_override, CASE WHEN t.demo = 1 THEN 'canary' ELSE 'stable' END) = 'canary'`;

/** Canary agents that reported the version, with their current module states. */
export async function gateAgents(db: D1Database, product: string, version: string): Promise<GateAgent[]> {
  const [agents, modules] = await db.batch<Record<string, unknown>>([
    db
      .prepare(
        `SELECT v.agent_id, v.first_seen, v.last_seen, v.rolled_back,
                (SELECT h.version FROM heartbeats h WHERE h.agent_id = v.agent_id) AS current_version
         ${CANARY_AGENTS}`,
      )
      .bind(product, version),
    db
      .prepare(`SELECT m.agent_id, m.name, m.state FROM agent_modules m WHERE m.agent_id IN (SELECT v.agent_id ${CANARY_AGENTS})`)
      .bind(product, version),
  ]);
  const byAgent = new Map<string, { name: string; state: string }[]>();
  for (const m of modules?.results ?? []) {
    const id = String(m.agent_id);
    byAgent.set(id, [...(byAgent.get(id) ?? []), { name: String(m.name), state: String(m.state) }]);
  }
  return (agents?.results ?? []).map((a) => ({
    agent_id: String(a.agent_id),
    first_seen: Number(a.first_seen),
    last_seen: Number(a.last_seen),
    rolled_back: Number(a.rolled_back) === 1,
    on_version: a.current_version === version,
    modules: byAgent.get(String(a.agent_id)) ?? [],
  }));
}

export async function fingerprintsFor(db: D1Database, version: string, errorsOnly: boolean): Promise<string[]> {
  const sql = errorsOnly
    ? `SELECT fingerprint FROM report_versions WHERE version = ?1 AND severity IN ('error', 'critical')`
    : `SELECT fingerprint FROM report_versions WHERE version = ?1`;
  const res = await db.prepare(sql).bind(version).all<{ fingerprint: string }>();
  return res.results.map((r) => r.fingerprint);
}

export async function insertPromotion(
  db: D1Database,
  p: { id: string; product: string; version: string; forced: boolean; reason: string | null; checks: string; promoted_at: number },
  stable: ReleaseRow,
): Promise<void> {
  await db.batch([
    db
      .prepare(
        `INSERT INTO releases (product, version, channel, manifest, published_at, created_at)
         VALUES (?1, ?2, 'stable', ?3, ?4, ?5)
         ON CONFLICT (product, version, channel) DO UPDATE SET manifest = excluded.manifest,
           published_at = excluded.published_at, created_at = excluded.created_at`,
      )
      .bind(stable.product, stable.version, stable.manifest, stable.published_at, stable.created_at),
    db
      .prepare(
        `INSERT INTO promotions (id, product, version, forced, reason, checks, promoted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)`,
      )
      .bind(p.id, p.product, p.version, p.forced ? 1 : 0, p.reason, p.checks, p.promoted_at),
  ]);
}

export async function listPromotions(db: D1Database, product: string, version: string): Promise<Record<string, unknown>[]> {
  const res = await db
    .prepare('SELECT * FROM promotions WHERE product = ?1 AND version = ?2 ORDER BY promoted_at DESC')
    .bind(product, version)
    .all<Record<string, unknown>>();
  return res.results;
}
