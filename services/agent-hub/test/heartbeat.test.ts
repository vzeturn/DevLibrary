import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { admin, enrollAgent, heartbeatBody, manifestText, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

let env: TestEnv;
let agent: TestAgent;

interface Answer {
  server_time: number;
  channel: string;
  heartbeat_seconds: number;
  config?: { version: number; body: unknown };
  commands: { id: string; kind: string; module?: string; args: unknown; expires_at: number }[];
  update_check: boolean;
}

async function beat(extra: Record<string, unknown> = {}, version = '1.0.0'): Promise<Answer> {
  const res = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent, version, extra));
  expect(res.status).toBe(200);
  return (await res.json()) as Answer;
}

beforeEach(async () => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
  agent = await enrollAgent(env, 'acme');
});
afterEach(() => vi.useRealTimers());

describe('POST /v1/heartbeat', () => {
  it('answers channel, cadence and server time, and stores the latest heartbeat', async () => {
    const a = await beat();
    expect(a).toEqual({ server_time: T0, channel: 'stable', heartbeat_seconds: 60, commands: [], update_check: false });
    const list = (await (await admin(env, 'GET', '/admin/agents')).json()) as { agents: Record<string, unknown>[] };
    expect(list.agents[0]).toMatchObject({ id: agent.id, online: true, version: '1.0.0', channel: 'stable', degraded_modules: [] });
    setNow(T0 + 181);
    const later = (await (await admin(env, 'GET', '/admin/agents')).json()) as { agents: Record<string, unknown>[] };
    expect(later.agents[0]?.online).toBe(false);
  });

  it('delivers each pending command exactly once and skips expired ones', async () => {
    const enqueue = async (kind: string, ttl: number): Promise<string> => {
      const res = await admin(env, 'POST', '/admin/commands', { agent_id: agent.id, kind, module: 'pda-sync', args: { level: 'debug' }, ttl_seconds: ttl });
      expect(res.status).toBe(201);
      return ((await res.json()) as { id: string }).id;
    };
    const live = await enqueue('set_log_level', 3600);
    await enqueue('restart', 10);
    setNow(T0 + 20);

    const first = await beat();
    expect(first.commands).toEqual([{ id: live, kind: 'set_log_level', module: 'pda-sync', args: { level: 'debug' }, expires_at: T0 + 3600 }]);
    expect((await beat()).commands).toEqual([]);

    const result = await signed(env, agent, 'POST', `/v1/commands/${live}/result`, { ok: true, output: 'done', finished_at: T0 + 21 });
    expect(result.status).toBe(200);
    expect(await result.json()).toEqual({});
    const dup = await signed(env, agent, 'POST', `/v1/commands/${live}/result`, { ok: true, output: 'again', finished_at: T0 + 22 });
    expect(dup.status).toBe(409);

    const listed = (await (await admin(env, 'GET', `/admin/commands?agent_id=${agent.id}`)).json()) as { commands: { kind: string; status: string; result_output: string | null }[] };
    expect(listed.commands.map((c) => [c.kind, c.status])).toEqual(
      expect.arrayContaining([['set_log_level', 'done'], ['restart', 'expired']]),
    );
  });

  it('accepts dotted module command kinds such as sys.ping', async () => {
    const res = await admin(env, 'POST', '/admin/commands', { agent_id: agent.id, kind: 'sys.ping', module: 'sys' });
    expect(res.status).toBe(201);
    expect((await beat()).commands.map((c) => [c.kind, c.module])).toEqual([['sys.ping', 'sys']]);
  });

  it('answers 404 for a result on another agent’s command', async () => {
    const other = await enrollAgent(env, 'other');
    const res = await admin(env, 'POST', '/admin/commands', { agent_id: other.id, kind: 'restart' });
    const { id } = (await res.json()) as { id: string };
    const answer = await signed(env, agent, 'POST', `/v1/commands/${id}/result`, { ok: true, output: '', finished_at: T0 });
    expect(answer.status).toBe(404);
  });

  it('pushes config only while the agent runs an older version', async () => {
    const put = await admin(env, 'PUT', `/admin/agents/${agent.id}/config`, { body: { modules: { 'pda-sync': { enabled: true } }, log_level: 'debug' } });
    expect(await put.json()).toMatchObject({ version: 1 });
    expect((await beat({ config_version: 0 })).config).toEqual({ version: 1, body: { modules: { 'pda-sync': { enabled: true } }, log_level: 'debug' } });
    expect((await beat({ config_version: 1 })).config).toBeUndefined();
    const second = await admin(env, 'PUT', `/admin/agents/${agent.id}/config`, { body: { modules: {}, log_level: 'info' } });
    expect(await second.json()).toMatchObject({ version: 2 });
    expect((await beat({ config_version: 1 })).config?.version).toBe(2);
  });

  it('sets update_check when the agent channel has a newer release', async () => {
    await admin(env, 'PUT', '/admin/releases/dl-agent/1.1.0', manifestText('1.1.0', 'canary', T0));
    expect((await beat()).update_check).toBe(false); // stable agent, release only on canary
    await admin(env, 'PUT', `/admin/agents/${agent.id}/channel`, { channel: 'canary' });
    const a = await beat();
    expect(a.channel).toBe('canary');
    expect(a.update_check).toBe(true);
    expect((await beat({}, '1.1.0')).update_check).toBe(false);
  });

  it('follows the release line of the product named in the heartbeat', async () => {
    await admin(env, 'PUT', `/admin/agents/${agent.id}/channel`, { channel: 'canary' });
    await admin(env, 'PUT', '/admin/releases/azolo-agent/1.1.0', manifestText('1.1.0', 'canary', T0, 'azolo-agent'));
    expect((await beat()).update_check).toBe(false); // default product dl-agent has no release
    expect((await beat({ product: 'azolo-agent' })).update_check).toBe(true);
    const versions = env.DB.raw.prepare('SELECT DISTINCT product FROM agent_versions ORDER BY product').all();
    expect(versions).toEqual([{ product: 'azolo-agent' }, { product: 'dl-agent' }]);
  });

  it('rejects a heartbeat naming another agent', async () => {
    const res = await signed(env, agent, 'POST', '/v1/heartbeat', { ...heartbeatBody(agent), agent_id: 'someone-else' });
    expect(res.status).toBe(400);
  });

  it('records module states and version history', async () => {
    await beat({ modules: [{ name: 'pda-sync', state: 'degraded', restarts: 2, last_error: 'sql down' }] });
    setNow(T0 + 120);
    await beat({}, '1.1.0');
    const versions = env.DB.raw.prepare('SELECT version, first_seen, last_seen FROM agent_versions ORDER BY version').all();
    expect(versions).toEqual([
      { version: '1.0.0', first_seen: T0, last_seen: T0 },
      { version: '1.1.0', first_seen: T0 + 120, last_seen: T0 + 120 },
    ]);
  });
});
