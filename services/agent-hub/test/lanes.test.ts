import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { bumpOf, DEFAULT_RAMP, gateRules, offeredPercent, parseRamp, rampPercent, rolloutBucket, type LaneConfig } from '../src/domain/lanes';
import {
  admin,
  createCode,
  createTenant,
  enrollAgent,
  enrollRequest,
  heartbeatBody,
  manifestText,
  newKeys,
  setNow,
  signed,
  T0,
  testEnv,
  type TestAgent,
  type TestEnv,
} from './helpers';

const H = 3600;
let env: TestEnv;

interface Answer {
  channel: string;
  commands: { kind: string }[];
  heartbeat_seconds: number;
  update_check: boolean;
}
interface Lane {
  bump: string;
  current_stable: string | null;
  promoted_at: number | null;
  rollout_percent: number | null;
  automatic: boolean;
  allowed: { tenant_id: string; site_id: string }[];
}
interface Gate {
  pass: boolean;
  min_agents: number;
  soak_hours: number;
  lane: Lane;
  checks: { name: string; ok: boolean }[];
}

async function beat(agent: TestAgent, version = '1.0.0', extra: Record<string, unknown> = {}): Promise<Answer> {
  const res = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent, version, extra));
  expect(res.status).toBe(200);
  return (await res.json()) as Answer;
}
async function gate(version: string): Promise<Gate> {
  const res = await admin(env, 'GET', `/admin/releases/dl-agent/${version}/gate`);
  expect(res.status).toBe(200);
  return (await res.json()) as Gate;
}
const failing = async (version: string): Promise<string[]> => (await gate(version)).checks.filter((c) => !c.ok).map((c) => c.name);
async function publish(version: string, at: number): Promise<void> {
  expect((await admin(env, 'PUT', `/admin/releases/dl-agent/${version}`, manifestText(version, 'canary', at))).status).toBe(200);
}
async function promote(version: string, at: number, body: Record<string, unknown> = {}, manifest = manifestText(version, 'stable', at)): Promise<Response> {
  return admin(env, 'POST', `/admin/releases/dl-agent/${version}/promote`, { manifest, ...body });
}
async function enrollAt(tenant: string, site: string): Promise<TestAgent> {
  await createTenant(env, tenant, false);
  const code = await createCode(env, tenant, site);
  const { keys, publicKey } = await newKeys();
  const res = await enrollRequest(env, { code, public_key: publicKey, hostname: 'h', os: 'windows', arch: 'x86_64', version: '1.0.0', modules: [] });
  return { id: ((await res.json()) as { agent_id: string }).agent_id, keys, publicKey };
}

describe('lanes: pure rules', () => {
  const cfg: LaneConfig = { minAgents: 1, soakHoursPatch: 2, soakHoursMinor: 24, soakHoursMajor: 72, ramp: DEFAULT_RAMP };

  it('measures the bump against the current stable', () => {
    expect(bumpOf(null, '1.0.0')).toBe('initial');
    expect(bumpOf('1.3.6', '1.3.7')).toBe('patch');
    expect(bumpOf('1.3.6', '1.3.7-rc.1')).toBe('patch');
    expect(bumpOf('1.3.6', '1.4.0')).toBe('minor');
    expect(bumpOf('1.3.6', '2.0.0')).toBe('major');
  });

  it('soaks by bump; a major needs two canary agents', () => {
    expect(gateRules(cfg, 'initial')).toEqual({ soakHours: 2, minAgents: 1 });
    expect(gateRules(cfg, 'patch')).toEqual({ soakHours: 2, minAgents: 1 });
    expect(gateRules(cfg, 'minor')).toEqual({ soakHours: 24, minAgents: 1 });
    expect(gateRules(cfg, 'major')).toEqual({ soakHours: 72, minAgents: 2 });
    expect(gateRules({ ...cfg, minAgents: 3 }, 'major').minAgents).toBe(3);
  });

  it('parses the minor ramp and falls back to the default on nonsense', () => {
    expect(parseRamp('0:5,12:100')).toEqual([{ afterHours: 0, percent: 5 }, { afterHours: 12, percent: 100 }]);
    for (const bad of ['', '0:10,24:50', '0:50,24:10,48:100', '24:10,0:100', 'x', '0:101']) expect(parseRamp(bad)).toBe(DEFAULT_RAMP);
    expect([0, 23 * H, 24 * H, 47 * H, 48 * H].map((t) => rampPercent(DEFAULT_RAMP, t))).toEqual([10, 10, 50, 50, 100]);
  });

  it('offers patch at once, minor by the ramp, never a major; the manifest is the ceiling', () => {
    expect(offeredPercent('patch', DEFAULT_RAMP, 0, 100)).toBe(100);
    expect(offeredPercent('minor', DEFAULT_RAMP, 30 * H, 100)).toBe(50);
    expect(offeredPercent('minor', DEFAULT_RAMP, 72 * H, 30)).toBe(30);
    expect(offeredPercent('major', DEFAULT_RAMP, 72 * H, 100)).toBeNull();
  });

  it('buckets agents exactly like dl-agent (channel.rs bucket_matches_the_hub)', async () => {
    expect(await rolloutBucket('agent-1', 'dl-agent', '1.4.0')).toBe(14);
  });
});

describe('lanes: hub', () => {
  let canary: TestAgent;

  beforeEach(async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    setNow(T0);
    env = testEnv();
    canary = await enrollAgent(env, 'demo', { demo: true });
    await publish('1.0.0', T0 - 10 * H);
    expect((await promote('1.0.0', T0 - 9 * H, { force: true, reason: 'bootstrap' })).status).toBe(200);
  });
  afterEach(() => vi.useRealTimers());

  it('canary: beats every 15 s (5 s on probation) and sees a publish on the next beat', async () => {
    const stable = await enrollAgent(env, 'acme');
    expect(await beat(canary)).toMatchObject({ channel: 'canary', heartbeat_seconds: 15, update_check: false });
    expect((await beat(stable)).heartbeat_seconds).toBe(60);
    setNow(T0 + 1);
    await publish('1.0.1', T0 + 1);
    // dl-agent acts on it in that same heartbeat (agent_lane_tests.rs): publish → install ≤ 15 s + download.
    setNow(T0 + 15);
    expect((await beat(canary)).update_check).toBe(true);
    expect((await beat(canary, '1.0.1', { update: { state: 'probation', version: '1.0.1' } })).heartbeat_seconds).toBe(5);
    expect((await beat(stable, '1.0.0', { update: { state: 'probation', version: '1.0.0' } })).heartbeat_seconds).toBe(60);
    env.CANARY_HEARTBEAT_SECONDS = '30';
    expect((await beat(canary, '1.0.1')).heartbeat_seconds).toBe(30);
  });

  it('patch: 2 h soak, then every stable agent at once', async () => {
    const stable = await enrollAgent(env, 'acme');
    await publish('1.0.1', T0);
    await beat(canary, '1.0.1');
    setNow(T0 + 2 * H);
    await beat(canary, '1.0.1');
    const g = await gate('1.0.1');
    expect(g).toMatchObject({ pass: true, soak_hours: 2, lane: { bump: 'patch', current_stable: '1.0.0', rollout_percent: null } });
    expect((await promote('1.0.1', T0 + 2 * H)).status).toBe(200);
    expect((await beat(stable)).update_check).toBe(true);
    expect((await gate('1.0.1')).lane).toMatchObject({ bump: 'patch', promoted_at: T0 + 2 * H, rollout_percent: 100, automatic: true });
    const stored = env.DB.raw.prepare('SELECT version, bump FROM promotions ORDER BY promoted_at').all();
    expect(stored).toEqual([
      { version: '1.0.0', bump: 'initial' },
      { version: '1.0.1', bump: 'patch' },
    ]);
  });

  it('minor: 24 h soak, then 10 → 50 → 100 % of stable agents by their rollout bucket', async () => {
    const agents = await Promise.all(Array.from({ length: 12 }, (_, i) => enrollAgent(env, `t${i}`)));
    await publish('1.1.0', T0);
    await beat(canary, '1.1.0');
    setNow(T0 + 2 * H);
    await beat(canary, '1.1.0');
    expect(await failing('1.1.0')).toEqual(['soak']);
    setNow(T0 + 24 * H);
    await beat(canary, '1.1.0');
    expect(await gate('1.1.0')).toMatchObject({ pass: true, soak_hours: 24, lane: { bump: 'minor' } });
    const P = T0 + 24 * H;
    expect((await promote('1.1.0', P)).status).toBe(200);

    const buckets = await Promise.all(agents.map((a) => rolloutBucket(a.id, 'dl-agent', '1.1.0')));
    for (const [hours, pct] of [[0, 10], [24, 50], [48, 100]] as const) {
      setNow(P + hours * H);
      const offered = await Promise.all(agents.map(async (a) => (await beat(a)).update_check));
      expect(offered).toEqual(buckets.map((b) => b < pct));
      expect((await gate('1.1.0')).lane.rollout_percent).toBe(pct);
    }
  });

  it('minor: the signed rollout_percent stays the ceiling', async () => {
    const agents = await Promise.all(Array.from({ length: 8 }, (_, i) => enrollAgent(env, `c${i}`)));
    await publish('1.1.0', T0);
    const capped = manifestText('1.1.0', 'stable', T0).replace('"rollout_percent":100', '"rollout_percent":30');
    expect((await promote('1.1.0', T0, { force: true, reason: 'test' }, capped)).status).toBe(200);
    setNow(T0 + 48 * H);
    const buckets = await Promise.all(agents.map((a) => rolloutBucket(a.id, 'dl-agent', '1.1.0')));
    const offered = await Promise.all(agents.map(async (a) => (await beat(a)).update_check));
    expect(offered).toEqual(buckets.map((b) => b < 30));
  });

  it('major: 72 h soak on two canary agents, then only opted-in tenants or sites', async () => {
    const acmeMain = await enrollAt('acme', 'main');
    const acmeAnnex = await enrollAt('acme', 'annex');
    const other = await enrollAt('globex', 'main');
    await publish('2.0.0', T0);
    await beat(canary, '2.0.0');
    setNow(T0 + 72 * H);
    await beat(canary, '2.0.0');
    expect(await gate('2.0.0')).toMatchObject({ pass: false, min_agents: 2, soak_hours: 72, lane: { bump: 'major', automatic: false } });
    expect(await failing('2.0.0')).toEqual(['min_agents', 'soak']);
    // Urgent and required do not open a major to everyone.
    const loud = manifestText('2.0.0', 'stable', T0).replace('"allow_downgrade":false', '"allow_downgrade":false,"urgent":true,"required":true');
    expect((await promote('2.0.0', T0, { force: true, reason: 'two canary PCs not ready' }, loud)).status).toBe(200);
    const offered = async (): Promise<boolean[]> => Promise.all([acmeMain, acmeAnnex, other].map(async (a) => (await beat(a)).update_check));
    expect(await offered()).toEqual([false, false, false]);

    const allow = (body: Record<string, unknown>, method = 'POST'): Promise<Response> => admin(env, method, '/admin/releases/dl-agent/2.0.0/allow', body);
    expect((await allow({ tenant_id: 'acme', site_id: 'annex' })).status).toBe(201);
    expect(await offered()).toEqual([false, true, false]);
    expect((await allow({ tenant_id: 'globex' })).status).toBe(201);
    expect(await offered()).toEqual([false, true, true]);
    expect((await gate('2.0.0')).lane).toMatchObject({
      rollout_percent: null,
      allowed: [
        { tenant_id: 'acme', site_id: 'annex' },
        { tenant_id: 'globex', site_id: '*' },
      ],
    });
    expect((await allow({ tenant_id: 'globex' }, 'DELETE')).status).toBe(200);
    expect((await allow({ tenant_id: 'globex' }, 'DELETE')).status).toBe(404);
    expect(await offered()).toEqual([false, true, false]);
    expect((await allow({ tenant_id: 'nobody' })).status).toBe(404);
    expect((await admin(env, 'POST', '/admin/releases/dl-agent/9.9.9/allow', { tenant_id: 'acme' })).status).toBe(404);
    // update_now still reaches any agent: the hub queues it, the agent installs whatever stable serves.
    expect((await admin(env, 'POST', '/admin/commands', { agent_id: acmeMain.id, kind: 'update_now' })).status).toBe(201);
    expect(await beat(acmeMain)).toMatchObject({ update_check: false, commands: [{ kind: 'update_now' }] });
  });

  it('promotions from before the lanes (no bump) still reach every stable agent', async () => {
    const stable = await enrollAgent(env, 'acme');
    await publish('2.0.0', T0);
    expect((await promote('2.0.0', T0, { force: true, reason: 'legacy' })).status).toBe(200);
    expect((await beat(stable)).update_check).toBe(false);
    env.DB.raw.prepare('UPDATE promotions SET bump = NULL').run();
    expect((await beat(stable)).update_check).toBe(true);
  });
});
