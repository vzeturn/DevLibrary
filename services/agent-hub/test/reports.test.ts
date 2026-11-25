import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { admin, enrollAgent, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

let env: TestEnv;
let agent: TestAgent;

const report = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
  fingerprint: '9f2c00aa11bb22cc',
  severity: 'error',
  module: 'pda-sync',
  code: 'sql_login_failed',
  message: 'login failed',
  context: { server: 'FO\\SQL2019' },
  first_seen: T0 - 100,
  last_seen: T0 - 10,
  count: 3,
  agent_version: '1.0.0',
  os: 'windows',
  hostname: 'FO-SERVER',
  ...over,
});

beforeEach(async () => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
  agent = await enrollAgent(env, 'acme');
});
afterEach(() => vi.useRealTimers());

describe('POST /v1/reports', () => {
  it('upserts by fingerprint: adds counts, keeps first_seen, keeps the latest version', async () => {
    const first = await signed(env, agent, 'POST', '/v1/reports', { reports: [report()] });
    expect(await first.json()).toEqual({ accepted: 1 });
    setNow(T0 + 60);
    const second = await signed(env, agent, 'POST', '/v1/reports', {
      reports: [report({ first_seen: T0 + 1, last_seen: T0 + 50, count: 2, agent_version: '1.1.0', command: 'update.install' })],
    });
    expect(second.status).toBe(200);

    const res = await admin(env, 'GET', '/admin/reports?tenant_id=acme');
    const { reports } = (await res.json()) as { reports: Record<string, unknown>[] };
    expect(reports).toHaveLength(1);
    expect(reports[0]).toMatchObject({
      agent_id: agent.id,
      count: 5,
      first_seen: T0 - 100,
      last_seen: T0 + 50,
      agent_version: '1.1.0',
      command: 'update.install',
      received_first_at: T0,
      received_last_at: T0 + 60,
      context: { server: 'FO\\SQL2019' },
    });
    const pairs = env.DB.raw.prepare('SELECT version FROM report_versions ORDER BY version').all();
    expect(pairs).toEqual([{ version: '1.0.0' }, { version: '1.1.0' }]);
  });

  it('stores the failed command and shows an empty one for agents that send none', async () => {
    await signed(env, agent, 'POST', '/v1/reports', {
      reports: [
        report({ fingerprint: 'aa11', command: 'restart_module', code: 'command_module_down' }),
        report({ fingerprint: 'bb22' }),
      ],
    });
    const res = await admin(env, 'GET', '/admin/reports?tenant_id=acme');
    const { reports } = (await res.json()) as { reports: { fingerprint: string; command: string }[] };
    const by = Object.fromEntries(reports.map((r) => [r.fingerprint, r.command]));
    expect(by).toEqual({ aa11: 'restart_module', bb22: '' });
    const bad = await signed(env, agent, 'POST', '/v1/reports', {
      reports: [report({ fingerprint: 'cc33', command: 'x'.repeat(201) })],
    });
    expect(bad.status).toBe(400);
  });

  it('filters by severity, fingerprint and version', async () => {
    await signed(env, agent, 'POST', '/v1/reports', {
      reports: [report(), report({ fingerprint: 'aaaa', severity: 'warning', agent_version: '1.1.0' })],
    });
    const get = async (q: string): Promise<number> =>
      ((await (await admin(env, 'GET', `/admin/reports?${q}`)).json()) as { reports: unknown[] }).reports.length;
    expect(await get('severity=warning')).toBe(1);
    expect(await get('fingerprint=9f2c00aa11bb22cc')).toBe(1);
    expect(await get('version=1.1.0')).toBe(1);
    expect(await get('tenant_id=other')).toBe(0);
  });

  it('rejects malformed reports', async () => {
    const res = await signed(env, agent, 'POST', '/v1/reports', { reports: [report({ severity: 'fatal' })] });
    expect(res.status).toBe(400);
  });
});
