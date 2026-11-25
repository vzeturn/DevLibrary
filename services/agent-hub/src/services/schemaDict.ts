import type { AgentRow, Env } from '../env';
import type { CoverageBody, ExpectedItem, SchemaSnapshotBody, SnapshotDatabase } from '../domain/schemaDict';
import { uuidv7 } from '../lib/crypto';
import { HttpError } from '../lib/http';
import { getAgent } from '../repo/agents';
import { getSnapshot, latestPerDatabase, listSnapshots, saveSnapshots, type SnapshotRow } from '../repo/schemaSnapshots';

export async function storeSchemaSnapshot(env: Env, agent: AgentRow, body: SchemaSnapshotBody, now: number): Promise<{ accepted: true; databases: number }> {
  const rows: SnapshotRow[] = body.databases.map((d, i) => ({
    id: uuidv7(now * 1000 + i),
    agent_id: agent.id,
    tenant_id: agent.tenant_id,
    site_id: agent.site_id,
    db_name: d.name,
    schema_hash: d.schema_hash,
    server_version: body.server_version,
    agent_version: body.agent_version,
    table_count: d.tables.length,
    routine_count: d.routines.length,
    payload_json: JSON.stringify(d),
    first_seen: now,
    last_seen: now,
  }));
  await saveSnapshots(env.DB, rows);
  return { accepted: true, databases: rows.length };
}

export async function snapshotList(env: Env, agentId: string | null): Promise<{ snapshots: Awaited<ReturnType<typeof listSnapshots>> }> {
  if (agentId && !(await getAgent(env.DB, agentId))) throw new HttpError(404, 'agent_not_found');
  return { snapshots: await listSnapshots(env.DB, agentId) };
}

export async function snapshotDetail(env: Env, id: string): Promise<Record<string, unknown>> {
  const row = await getSnapshot(env.DB, id);
  if (!row) throw new HttpError(404, 'snapshot_not_found');
  const { payload_json, ...meta } = row;
  return { ...meta, database: JSON.parse(payload_json) as SnapshotDatabase };
}

export type CoverageStatus = 'present' | 'missing' | 'drift';

export interface CoverageCell {
  feature: string;
  object: string;
  status: CoverageStatus;
  missing_columns: string[];
}

export function judge(item: ExpectedItem, db: SnapshotDatabase | undefined): CoverageCell {
  const lower = (s: string): string => s.toLowerCase();
  if (item.routine !== undefined) {
    const found = db?.routines.some((r) => lower(r.schema) === lower(item.schema) && lower(r.name) === lower(item.routine ?? ''));
    return { feature: item.feature, object: `${item.database}.${item.schema}.${item.routine}`, status: found ? 'present' : 'missing', missing_columns: [] };
  }
  const t = db?.tables.find((x) => lower(x.schema) === lower(item.schema) && lower(x.name) === lower(item.table ?? ''));
  const object = `${item.database}.${item.schema}.${item.table}`;
  if (!t) return { feature: item.feature, object, status: 'missing', missing_columns: [] };
  const have = new Set(t.columns.map((c) => lower(c.name)));
  const missing = item.columns.filter((c) => !have.has(lower(c)));
  return { feature: item.feature, object, status: missing.length > 0 ? 'drift' : 'present', missing_columns: missing };
}

/**
 * Coverage matrix: each expected table/column/routine of a feature against the newest snapshot of
 * the matching database, per agent. `present` = there, `drift` = table there with columns missing,
 * `missing` = absent (or the database was never snapshotted). Percent counts only `present`.
 */
export async function coverage(env: Env, body: CoverageBody): Promise<Record<string, unknown>> {
  const snaps = await latestPerDatabase(env.DB, body.agent_ids ?? []);
  const byAgent = new Map<string, Map<string, SnapshotRow>>();
  for (const s of snaps) {
    if (!byAgent.has(s.agent_id)) byAgent.set(s.agent_id, new Map());
    byAgent.get(s.agent_id)?.set(s.db_name.toLowerCase(), s);
  }
  for (const id of body.agent_ids ?? []) if (!byAgent.has(id)) byAgent.set(id, new Map());
  const sites = [...byAgent.entries()].map(([agentId, dbs]) => {
    const parsed = new Map<string, SnapshotDatabase>();
    const cells = body.expected.map((item) => {
      const key = item.database.toLowerCase();
      if (!parsed.has(key) && dbs.has(key)) parsed.set(key, JSON.parse(dbs.get(key)?.payload_json ?? '{}') as SnapshotDatabase);
      return judge(item, parsed.get(key));
    });
    const count = (s: CoverageStatus): number => cells.filter((c) => c.status === s).length;
    const first = [...dbs.values()][0];
    return {
      agent_id: agentId,
      site_id: first?.site_id ?? '',
      server_version: first?.server_version ?? '',
      databases: [...dbs.values()].map((d) => ({ name: d.db_name, schema_hash: d.schema_hash })),
      present: count('present'),
      drift: count('drift'),
      missing: count('missing'),
      total: cells.length,
      percent: Math.round((count('present') / cells.length) * 1000) / 10,
      cells,
    };
  });
  return { sites };
}
