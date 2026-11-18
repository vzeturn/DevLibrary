import type { Channel } from '../env';
import type { HeartbeatBody } from '../domain/schemas';

/** Stores the latest heartbeat, module states and version history in one transaction. */
export async function saveHeartbeat(
  db: D1Database,
  agentId: string,
  hb: HeartbeatBody,
  channel: Channel,
  rawBody: string,
  now: number,
): Promise<void> {
  const stmts: D1PreparedStatement[] = [
    db
      .prepare(
        `INSERT INTO heartbeats (agent_id, received_at, version, channel, body) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (agent_id) DO UPDATE SET received_at = excluded.received_at, version = excluded.version,
           channel = excluded.channel, body = excluded.body`,
      )
      .bind(agentId, now, hb.version, channel, rawBody),
    db.prepare('DELETE FROM agent_modules WHERE agent_id = ?1').bind(agentId),
    ...hb.modules.map((m) =>
      db
        .prepare(
          `INSERT INTO agent_modules (agent_id, name, state, detail, restarts, last_error, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)`,
        )
        .bind(agentId, m.name, m.state, m.detail ?? null, m.restarts, m.last_error ?? null, now),
    ),
    db
      .prepare(
        `INSERT INTO agent_versions (agent_id, product, version, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT (agent_id, product, version) DO UPDATE SET last_seen = excluded.last_seen`,
      )
      .bind(agentId, hb.product, hb.version, now),
    db
      .prepare('UPDATE agents SET version = ?2, last_seen_at = ?3, hostname = COALESCE(?4, hostname) WHERE id = ?1')
      .bind(agentId, hb.version, now, hb.hostname ?? null),
  ];
  if (hb.update?.state === 'rolled_back' && hb.update.version && hb.update.version !== hb.version) {
    stmts.push(
      db
        .prepare(
          `INSERT INTO agent_versions (agent_id, product, version, first_seen, last_seen, rolled_back)
           VALUES (?1, ?2, ?3, ?4, ?4, 1)
           ON CONFLICT (agent_id, product, version) DO UPDATE SET rolled_back = 1`,
        )
        .bind(agentId, hb.product, hb.update.version, now),
    );
  }
  await db.batch(stmts);
}
