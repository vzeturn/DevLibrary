import type { AgentRow, Channel } from '../env';

export interface AgentListRow extends AgentRow {
  tenant_demo: number;
  heartbeat_at: number | null;
  heartbeat_version: string | null;
  heartbeat_body: string | null;
  degraded_modules: string | null;
}

export async function getAgent(db: D1Database, id: string): Promise<AgentRow | null> {
  return db.prepare('SELECT * FROM agents WHERE id = ?1').bind(id).first<AgentRow>();
}

export async function getTenantDemo(db: D1Database, tenantId: string): Promise<boolean> {
  const demo = await db.prepare('SELECT demo FROM tenants WHERE id = ?1').bind(tenantId).first<number>('demo');
  return demo === 1;
}

export async function listAgents(db: D1Database, tenantId: string | undefined, agentId?: string): Promise<AgentListRow[]> {
  const sql = `
    SELECT a.*, t.demo AS tenant_demo, h.received_at AS heartbeat_at, h.version AS heartbeat_version,
           h.body AS heartbeat_body,
           (SELECT group_concat(m.name || '=' || m.state, ', ') FROM agent_modules m
             WHERE m.agent_id = a.id AND m.state NOT IN ('running', 'disabled')) AS degraded_modules
    FROM agents a
    JOIN tenants t ON t.id = a.tenant_id
    LEFT JOIN heartbeats h ON h.agent_id = a.id
    WHERE (?1 IS NULL OR a.tenant_id = ?1) AND (?2 IS NULL OR a.id = ?2)
    ORDER BY a.tenant_id, a.site_id, a.hostname`;
  return (await db.prepare(sql).bind(tenantId ?? null, agentId ?? null).all<AgentListRow>()).results;
}

export async function setChannelOverride(db: D1Database, id: string, channel: Channel | null): Promise<boolean> {
  const res = await db.prepare('UPDATE agents SET channel_override = ?2 WHERE id = ?1').bind(id, channel).run();
  return res.meta.changes === 1;
}

export async function revokeAgent(db: D1Database, id: string, now: number): Promise<boolean> {
  const res = await db
    .prepare('UPDATE agents SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL')
    .bind(id, now)
    .run();
  return res.meta.changes === 1;
}

/** Records a nonce; false when it was already used inside the replay window. */
export async function recordNonce(db: D1Database, agentId: string, nonce: string, now: number, ttl: number): Promise<boolean> {
  const [, insert] = await db.batch([
    db.prepare('DELETE FROM nonces WHERE agent_id = ?1 AND seen_at < ?2').bind(agentId, now - ttl),
    db
      .prepare('INSERT INTO nonces (agent_id, nonce, seen_at) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING')
      .bind(agentId, nonce, now),
  ]);
  return (insert?.meta.changes ?? 0) === 1;
}

export async function pruneNonces(db: D1Database, before: number): Promise<number> {
  const res = await db.prepare('DELETE FROM nonces WHERE seen_at < ?1').bind(before).run();
  return res.meta.changes;
}
