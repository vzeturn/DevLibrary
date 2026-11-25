import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { admin, enrollAgent, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

let env: TestEnv;
let agent: TestAgent;

const col = (name: string) => ({ name, sql_type: 'int', max_length: 4, nullable: false, identity: false });
const db = (name: string, hash: string, tables: [string, string[]][], routines: string[] = []) => ({
  name,
  schema_hash: hash.repeat(64).slice(0, 64),
  tables: tables.map(([t, cols]) => ({ schema: 'dbo', name: t, est_rows: 10, columns: cols.map(col), indexes: [] })),
  foreign_keys: [],
  routines: routines.map((r) => ({ schema: 'dbo', name: r, kind: 'P' })),
});
const snap = (databases: unknown[], over: Record<string, unknown> = {}) => ({ agent_version: '1.2.0', server_version: 'SQL Server 2016', databases, ...over });
const upload = (body: unknown, who: TestAgent = agent): Promise<Response> => signed(env, who, 'POST', '/v1/agent/schema-snapshot', body);
const coverage = async (body: unknown) => (await (await admin(env, 'POST', '/admin/schema/coverage', body)).json()) as { sites: Record<string, any>[] };

beforeEach(async () => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
  agent = await enrollAgent(env, 'acme');
});
afterEach(() => vi.useRealTimers());

describe('POST /v1/agent/schema-snapshot', () => {
  it('stores one row per database; the same hash again does not duplicate', async () => {
    const body = snap([db('SMILE_FO', 'a', [['Folio', ['Id']]]), db('SMILE_POS', 'b', [['Check', ['Id']]])]);
    expect(await (await upload(body)).json()).toEqual({ accepted: true, databases: 2 });
    await upload(body);
    const list = (await (await admin(env, 'GET', `/admin/schema/snapshots?agent_id=${agent.id}`)).json()) as { snapshots: { db_name: string; table_count: number }[] };
    expect(list.snapshots.map((s) => s.db_name).sort()).toEqual(['SMILE_FO', 'SMILE_POS']);
    expect(list.snapshots[0]?.table_count).toBe(1);
  });

  it('keeps a new version beside the old one and the detail returns the structure', async () => {
    await upload(snap([db('SMILE_FO', 'a', [['Folio', ['Id']]])]));
    await upload(snap([db('SMILE_FO', 'c', [['Folio', ['Id', 'Note']]])], { server_version: 'SQL Server 2019' }));
    const list = (await (await admin(env, 'GET', '/admin/schema/snapshots')).json()) as { snapshots: { id: string; server_version: string }[] };
    expect(list.snapshots).toHaveLength(2);
    const detail = (await (await admin(env, 'GET', `/admin/schema/snapshots/${list.snapshots[0]?.id}`)).json()) as { database: { tables: unknown[] } };
    expect(detail.database.tables).toHaveLength(1);
  });

  it('refuses a field that could carry customer rows, an unsigned call and a bad hash', async () => {
    const withRows = snap([{ ...db('SMILE_FO', 'a', [['Folio', ['Id']]]), rows: [{ Id: 1 }] }]);
    expect((await upload(withRows)).status).toBe(400);
    expect((await upload(snap([{ ...db('X', 'a', []), schema_hash: 'nothex' }]))).status).toBe(400);
    expect((await upload(snap([]))).status).toBe(400);
    const forged = await signed(env, agent, 'POST', '/v1/agent/schema-snapshot', snap([db('X', 'a', [])]), { agentId: 'someone-else' });
    expect(forged.status).toBe(401);
  });

  it('answers 404 for an unknown agent or snapshot', async () => {
    expect((await admin(env, 'GET', '/admin/schema/snapshots?agent_id=nope')).status).toBe(404);
    expect((await admin(env, 'GET', '/admin/schema/snapshots/nope')).status).toBe(404);
  });
});

describe('POST /admin/schema/coverage', () => {
  const expected = [
    { feature: 'Folio', database: 'smile_fo', table: 'folio', columns: ['id'] },
    { feature: 'Folio notes', database: 'SMILE_FO', table: 'Folio', columns: ['Id', 'Note'] },
    { feature: 'Posting', database: 'SMILE_FO', routine: 'PostPos' },
    { feature: 'Checks', database: 'SMILE_POS', table: 'Check' },
  ];

  it('marks present, drift and missing and gives the percent per site, newest snapshot only', async () => {
    await upload(snap([db('SMILE_FO', 'a', [['Folio', ['Id']]], ['PostPos'])]));
    const r = await coverage({ expected });
    const site = r.sites[0]!;
    expect(site.cells.map((c: any) => c.status)).toEqual(['present', 'drift', 'present', 'missing']);
    expect(site.cells[1].missing_columns).toEqual(['Note']);
    expect(site).toMatchObject({ present: 2, drift: 1, missing: 1, total: 4, percent: 50 });
  });

  it('compares sites of different versions side by side', async () => {
    const other = await enrollAgent(env, 'beta');
    await upload(snap([db('SMILE_FO', 'a', [['Folio', ['Id']]], ['PostPos'])]));
    await upload(snap([db('SMILE_FO', 'c', [['Folio', ['Id', 'Note']]], ['PostPos']), db('SMILE_POS', 'd', [['Check', ['Id']]])], { server_version: 'SQL Server 2019' }), other);
    const r = await coverage({ expected });
    const pct = Object.fromEntries(r.sites.map((s) => [s.agent_id, s.percent]));
    expect(pct[agent.id]).toBe(50);
    expect(pct[other.id]).toBe(100);
  });

  it('lists a requested agent that never uploaded as all missing, and filters by agent_ids', async () => {
    const other = await enrollAgent(env, 'beta');
    await upload(snap([db('SMILE_FO', 'a', [['Folio', ['Id']]], ['PostPos'])]));
    const r = await coverage({ expected, agent_ids: [other.id] });
    expect(r.sites).toHaveLength(1);
    expect(r.sites[0]).toMatchObject({ agent_id: other.id, missing: 4, percent: 0 });
  });

  it('rejects an item with both table and routine, and an empty list', async () => {
    expect((await admin(env, 'POST', '/admin/schema/coverage', { expected: [{ feature: 'x', database: 'd', table: 't', routine: 'r' }] })).status).toBe(400);
    expect((await admin(env, 'POST', '/admin/schema/coverage', { expected: [] })).status).toBe(400);
  });
});
