import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { admin, enrollAgent, heartbeatBody, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

let env: TestEnv;
let agent: TestAgent;

const check = (status: 'pass' | 'warn' | 'fail' | 'skip', id: string, extra: Record<string, unknown> = {}): Record<string, unknown> => ({
  module: 'einvoice',
  id,
  title: `Check ${id}`,
  status,
  detail: `${id} detail`,
  duration_ms: 12,
  ...extra,
});

const run = (runId: string, over: Record<string, unknown> = {}): Record<string, unknown> => ({
  run_id: runId,
  trigger: 'install',
  started_at: T0 - 5,
  finished_at: T0,
  agent_version: '1.2.0',
  os: 'windows',
  hostname: 'TEST-PC',
  checks: [
    check('pass', 'sql'),
    check('pass', 'hub'),
    check('warn', 'clock'),
    check('fail', 'token', { title: 'Provider token', detail: 'expired <b>', remedy: 'run einvoice-enroll' }),
    check('skip', 'printer'),
  ],
  ...over,
});

const upload = async (body: unknown, who: TestAgent = agent): Promise<Response> => signed(env, who, 'POST', '/v1/agent/selftest', body);

interface View {
  agent_id: string;
  latest: { run_id: string; trigger: string; summary: Record<string, unknown>; checks: Record<string, unknown>[] } | null;
  history: { run_id: string; summary: { fail: number } }[];
}

const view = async (id = agent.id): Promise<View> => (await (await admin(env, 'GET', `/admin/agents/${id}/selftest`)).json()) as View;

beforeEach(async () => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
  agent = await enrollAgent(env, 'acme');
});
afterEach(() => vi.useRealTimers());

describe('POST /v1/agent/selftest', () => {
  it('stores a signed run with its counts; the admin view shows the latest with checks', async () => {
    const res = await upload(run('r1'));
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ accepted: true, run_id: 'r1' });

    const v = await view();
    expect(v.latest).toMatchObject({ run_id: 'r1', trigger: 'install', summary: { pass: 2, warn: 1, fail: 1, skip: 1, total: 5, os: 'windows', hostname: 'TEST-PC' } });
    expect(v.latest?.checks).toHaveLength(5);
    expect(v.latest?.checks[3]).toMatchObject({ id: 'token', status: 'fail', remedy: 'run einvoice-enroll' });
    expect(v.history.map((h) => h.run_id)).toEqual(['r1']);
  });

  it('needs a valid signature', async () => {
    const other = await enrollAgent(env, 'other');
    const forged = await signed(env, agent, 'POST', '/v1/agent/selftest', run('r1'), { signWith: other.keys });
    expect(forged.status).toBe(401);
    expect((await view()).latest).toBeNull();
  });

  it.each([
    ['an unknown status', run('r1', { checks: [check('pass', 'a', { status: 'ok' })] })],
    ['a missing run_id', run('r1', { run_id: undefined })],
    ['a bad trigger', run('r1', { trigger: 'Install Now' })],
    ['over 500 checks', run('r1', { checks: Array.from({ length: 501 }, (_, i) => check('pass', `c${i}`)) })],
    ['a negative duration', run('r1', { checks: [check('pass', 'a', { duration_ms: -1 })] })],
  ])('rejects %s', async (_name, body) => {
    expect((await upload(body)).status).toBe(400);
  });

  it('refuses a body over 256 KB', async () => {
    const big = run('r1', { checks: Array.from({ length: 80 }, (_, i) => check('pass', `c${i}`, { detail: 'x'.repeat(3500) })) });
    const res = await upload(big);
    expect(res.status).toBe(413);
    expect(await res.json()).toEqual({ error: 'selftest_too_large' });
  });

  it('replaces a retried run_id and keeps the newest 20 runs per agent', async () => {
    const other = await enrollAgent(env, 'other');
    expect((await upload(run('keep'), other)).status).toBe(200);
    for (let i = 1; i <= 22; i++) {
      setNow(T0 + i);
      expect((await upload(run(`r${i}`))).status).toBe(200);
    }
    setNow(T0 + 30);
    expect((await upload(run('r22', { checks: [check('pass', 'only')] }))).status).toBe(200);

    const v = await view();
    expect(v.history).toHaveLength(20);
    expect(v.history[0]?.run_id).toBe('r22');
    expect(v.history.at(-1)?.run_id).toBe('r3');
    expect(v.latest?.summary).toMatchObject({ pass: 1, fail: 0, total: 1 });
    expect((await view(other.id)).history.map((h) => h.run_id)).toEqual(['keep']);
  });

  it('answers 404 for an unknown agent and null before the first run', async () => {
    expect((await admin(env, 'GET', '/admin/agents/nope/selftest')).status).toBe(404);
    expect(await view()).toEqual({ agent_id: agent.id, latest: null, history: [] });
  });
});

describe('status page and the selftest command', () => {
  it('shows the latest run per agent with counts and failed checks, escaped', async () => {
    const empty = await (await admin(env, 'GET', '/admin')).text();
    expect(empty).toContain('Tự kiểm tra');
    expect(empty).toContain('Chưa agent nào gửi kết quả tự kiểm tra.');

    await upload(run('r1'));
    setNow(T0 + 60);
    await upload(run('r2', { trigger: 'command' }));
    const page = await (await admin(env, 'GET', '/admin')).text();
    expect(page).toContain('(command)');
    expect(page).not.toContain('(install)');
    expect(page).toContain('einvoice · Provider token');
    expect(page).toContain('expired &lt;b&gt;');
    expect(page).toContain('run einvoice-enroll');
    expect(page).not.toContain('Check sql');
  });

  it('a `selftest` command goes through the generic command path to the next heartbeat', async () => {
    const res = await admin(env, 'POST', '/admin/commands', { agent_id: agent.id, kind: 'selftest', args: { trigger: 'command' } });
    expect(res.status).toBe(201);
    const { id } = (await res.json()) as { id: string };
    const beat = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent));
    const { commands } = (await beat.json()) as { commands: { id: string; kind: string; args: unknown }[] };
    expect(commands).toEqual([expect.objectContaining({ id, kind: 'selftest', args: { trigger: 'command' } })]);
  });
});
