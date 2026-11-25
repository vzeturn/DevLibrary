import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { evaluateWatch, type WatchAgent, type WatchInput } from '../src/domain/watch';
import { admin, enrollAgent, heartbeatBody, manifestText, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

const H = 3600;
let env: TestEnv;

describe('site canary watch: pure rules', () => {
  const ok: WatchAgent = { agent_id: 'a', silent_seconds: 30, on_version: true, rolled_back: false, selftest_failures: 0, modules: [{ name: 'pms', state: 'running' }] };
  const input = (over: Partial<WatchInput> = {}): WatchInput => ({ watchHours: 4, elapsedSeconds: 4 * H, silentLimitSeconds: 900, agents: [ok], newErrors: [], ...over });

  it('is good only after the watch time with a healthy site on the version', () => {
    expect(evaluateWatch(input()).verdict).toBe('good');
    expect(evaluateWatch(input({ elapsedSeconds: 4 * H - 1 }))).toMatchObject({ verdict: 'waiting' });
    expect(evaluateWatch(input({ agents: [{ ...ok, on_version: false }] }))).toMatchObject({ verdict: 'waiting' });
  });

  it('never strands a release on an empty lane', () => {
    expect(evaluateWatch(input({ agents: [] })).verdict).toBe('good');
  });

  it.each([
    ['a rollback', { rolled_back: true, on_version: false }, 'rolled back'],
    ['silence', { silent_seconds: 901 }, 'silent'],
    ['a failed self-test', { selftest_failures: 2 }, 'self-test'],
    ['a module that is not running', { modules: [{ name: 'pms', state: 'crashed' }] }, 'pms: crashed'],
  ])('is bad at once on %s, before the watch time ends', (_n, patch, text) => {
    const r = evaluateWatch(input({ elapsedSeconds: 60, agents: [{ ...ok, ...patch }] }));
    expect(r.verdict).toBe('bad');
    expect(r.reasons.join(' ')).toContain(text);
  });

  it('is bad on a new error fingerprint, and a silent site that never took the release is not a fault', () => {
    expect(evaluateWatch(input({ newErrors: ['f2', 'f1'] })).reasons).toEqual(['new error fingerprints: f1, f2']);
    expect(evaluateWatch(input({ agents: [ok, { ...ok, agent_id: 'b', on_version: false, silent_seconds: 99999 }] })).verdict).toBe('good');
  });
});

describe('site canary lane: hub', () => {
  let site: TestAgent;
  let other: TestAgent;
  const P = T0 + 2 * H;

  async function beat(a: TestAgent, version = '1.0.0', extra: Record<string, unknown> = {}): Promise<{ update_check: boolean }> {
    const res = await signed(env, a, 'POST', '/v1/heartbeat', heartbeatBody(a, version, extra));
    expect(res.status).toBe(200);
    return (await res.json()) as { update_check: boolean };
  }
  const publish = async (v: string, at: number): Promise<void> => {
    expect((await admin(env, 'PUT', `/admin/releases/dl-agent/${v}`, manifestText(v, 'canary', at))).status).toBe(200);
  };
  const promote = (v: string, at: number): Promise<Response> => admin(env, 'POST', `/admin/releases/dl-agent/${v}/promote`, { manifest: manifestText(v, 'stable', at), force: true, reason: 'test' });
  const served = async (a: TestAgent): Promise<string> => {
    const res = await signed(env, a, 'GET', '/v1/releases/dl-agent/stable');
    return (JSON.parse(await res.text()) as { version: string }).version;
  };

  beforeEach(async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    setNow(T0);
    env = testEnv();
    await publish('1.0.0', T0 - 10 * H);
    expect((await promote('1.0.0', T0 - 9 * H)).status).toBe(200);
    site = await enrollAgent(env, 'acme');
    other = await enrollAgent(env, 'beta');
    await beat(site);
    await beat(other);
    expect((await admin(env, 'POST', '/admin/site-canaries', { tenant_id: 'acme' })).status).toBe(201);
    setNow(T0 + H);
    await publish('1.0.1', T0 + H);
    setNow(P);
    expect((await promote('1.0.1', P)).status).toBe(200);
  });
  afterEach(() => vi.useRealTimers());

  it('offers the release to the listed site only while it is watched', async () => {
    expect((await beat(site)).update_check).toBe(true);
    expect((await beat(other)).update_check).toBe(false);
    setNow(P + 3 * H);
    await beat(site, '1.0.1');
    expect((await beat(other)).update_check).toBe(false);
  });

  it('opens to the ramp after the watch time when the site stayed healthy', async () => {
    setNow(P + H);
    await beat(site, '1.0.1');
    setNow(P + 4 * H);
    await beat(site, '1.0.1');
    expect((await beat(other)).update_check).toBe(true);
    expect(env.DB.raw.prepare('SELECT verdict FROM release_watch').all()).toEqual([{ verdict: 'good' }]);
  });

  it('pulls the release back when the site rolled it back: paused, previous stable served', async () => {
    setNow(P + H);
    await beat(site, '1.0.1');
    await beat(site, '1.0.0', { update: { state: 'rolled_back', version: '1.0.1' } });
    expect((await beat(other)).update_check).toBe(false);
    expect(await served(other)).toBe('1.0.0');
    expect(env.DB.raw.prepare('SELECT verdict FROM release_watch').all()).toEqual([{ verdict: 'bad' }]);
    expect(env.DB.raw.prepare('SELECT reason FROM release_pauses').all()).toEqual([{ reason: expect.stringContaining('rolled back') }]);
    setNow(P + 10 * H);
    expect((await beat(other)).update_check).toBe(false);
  });

  it('pulls it back when the site goes silent after taking the release', async () => {
    setNow(P + H);
    await beat(site, '1.0.1');
    setNow(P + H + 1000);
    expect((await beat(other)).update_check).toBe(false);
    expect(env.DB.raw.prepare('SELECT reasons FROM release_watch').all()).toEqual([{ reasons: expect.stringContaining('silent') }]);
    expect(await served(other)).toBe('1.0.0');
  });

  it('pulls it back on a failed self-test of the new version', async () => {
    setNow(P + H);
    await beat(site, '1.0.1');
    env.DB.raw
      .prepare(`INSERT INTO selftest_runs (id, agent_id, run_id, trigger, started_at, finished_at, agent_version, summary_json, checks_json, received_at)
                VALUES ('s1', ?1, 'r1', 'manual', ?2, ?2, '1.0.1', '{"pass":3,"fail":1}', '[]', ?2)`)
      .run(site.id, P + H);
    expect((await beat(other)).update_check).toBe(false);
    expect(env.DB.raw.prepare('SELECT verdict FROM release_watch').all()).toEqual([{ verdict: 'bad' }]);
  });

  it('resuming a pulled-back release clears the verdict; the fallback stable is not judged', async () => {
    setNow(P + H);
    await beat(site, '1.0.1');
    await beat(site, '1.0.0', { update: { state: 'rolled_back', version: '1.0.1' } });
    expect((await beat(other)).update_check).toBe(false);
    expect((await admin(env, 'POST', '/admin/releases/dl-agent/1.0.1/resume')).status).toBe(200);
    expect(env.DB.raw.prepare('SELECT COUNT(*) AS n FROM release_watch').all()).toEqual([{ n: 0 }]);
  });

  it('does not judge a promotion made before the lane existed', async () => {
    await publish('1.0.2', T0);
    setNow(T0 - 1);
    // Promoted before the list was created (T0): behaves as before, every stable agent at once.
    env.DB.raw.prepare('UPDATE site_canaries SET created_at = ?1').run(P + 100 * H);
    setNow(P + 100 * H);
    expect((await beat(other)).update_check).toBe(true);
  });

  it('manages the list through the admin API', async () => {
    const list = (await (await admin(env, 'GET', '/admin/site-canaries')).json()) as { site_canaries: { tenant_id: string }[] };
    expect(list.site_canaries.map((s) => s.tenant_id)).toEqual(['acme']);
    expect((await admin(env, 'POST', '/admin/site-canaries', { tenant_id: 'nope' })).status).toBe(404);
    expect((await admin(env, 'DELETE', '/admin/site-canaries', { tenant_id: 'acme' })).status).toBe(200);
    expect((await admin(env, 'DELETE', '/admin/site-canaries', { tenant_id: 'acme' })).status).toBe(404);
  });
});
