import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { evaluateCanaryHealth, type CanaryHealthAgent, type CanaryHealthConfig, type CanaryHealthInput } from '../src/domain/canary-health';
import { evaluateGate } from '../src/domain/gate';
import { watchCanary } from '../src/services/lanes';
import { createTenant, enrollAgent, setNow, T0, testEnv, type TestEnv } from './helpers';

const H = 3600;
const config: CanaryHealthConfig = { windowHours: 4, maxMissedBeats: 5, maxErrorsPerHour: 5 };
const NOW = 10 * H;

const agent = (over: Partial<CanaryHealthAgent> = {}): CanaryHealthAgent => ({
  agent_id: 'a1',
  installed_at: NOW - 5 * H,
  last_heartbeat: NOW - 10,
  heartbeat_seconds: 15,
  rolled_back: false,
  selftest: { fail: 0 },
  error_count: 0,
  ...over,
});
const input = (agents: CanaryHealthAgent[]): CanaryHealthInput => ({ now: NOW, config, agents, previousVersion: '1.0.0' });

describe('evaluateCanaryHealth', () => {
  it('is healthy and promotable when every signal is good for the whole window', () => {
    const r = evaluateCanaryHealth(input([agent(), agent({ agent_id: 'a2', error_count: 3 })]));
    expect(r).toMatchObject({ verdict: 'healthy', promotable: true, rollbackTo: null, reasons: [] });
  });

  it('rolls back on missed heartbeats, even inside the window', () => {
    const r = evaluateCanaryHealth(input([agent({ installed_at: NOW - H, last_heartbeat: NOW - 15 * 5 })]));
    expect(r.verdict).toBe('rollback');
    expect(r.promotable).toBe(false);
    expect(r.rollbackTo).toBe('1.0.0');
    expect(r.reasons[0]).toContain('5 heartbeats missed');
  });

  it('tolerates fewer missed heartbeats than the limit', () => {
    expect(evaluateCanaryHealth(input([agent({ last_heartbeat: NOW - 15 * 4 - 14 })])).verdict).toBe('healthy');
  });

  it('rolls back on a failed selftest', () => {
    const r = evaluateCanaryHealth(input([agent({ selftest: { fail: 2 } })]));
    expect(r).toMatchObject({ verdict: 'rollback', promotable: false, rollbackTo: '1.0.0' });
    expect(r.reasons[0]).toContain('selftest failed');
  });

  it('rolls back on an error spike over the threshold', () => {
    const r = evaluateCanaryHealth(input([agent({ installed_at: NOW - 2 * H, error_count: 11 })]));
    expect(r.verdict).toBe('rollback');
    expect(r.reasons[0]).toContain('error rate');
    expect(evaluateCanaryHealth(input([agent({ installed_at: NOW - 2 * H, error_count: 10 })])).verdict).toBe('pending');
  });

  it('stays pending, and blocks promotion, while the window is not over', () => {
    const r = evaluateCanaryHealth(input([agent({ installed_at: NOW - 3 * H })]));
    expect(r).toMatchObject({ verdict: 'pending', promotable: false, rollbackTo: null });
    expect(r.reasons[0]).toContain('3.0h of 4h');
  });

  it('is pending when no canary agent has the release', () => {
    expect(evaluateCanaryHealth(input([])).verdict).toBe('pending');
  });

  it('treats an agent-side rollback as bad', () => {
    expect(evaluateCanaryHealth(input([agent({ rolled_back: true })])).verdict).toBe('rollback');
  });
});

describe('gate canary_health check', () => {
  const base = {
    minAgents: 1,
    soakHours: 2,
    agents: [{ agent_id: 'a1', first_seen: 0, last_seen: 3 * H, rolled_back: false, on_version: true, modules: [] }],
    errorFingerprints: [],
    previousVersion: '1.0.0',
    previousFingerprints: [],
  };
  it('blocks promotion unless the verdict is healthy', () => {
    const bad = evaluateGate({ ...base, canaryHealth: evaluateCanaryHealth(input([agent({ selftest: { fail: 1 } })])) });
    expect(bad.pass).toBe(false);
    expect(bad.checks.find((c) => c.name === 'canary_health')?.ok).toBe(false);
    const pending = evaluateGate({ ...base, canaryHealth: evaluateCanaryHealth(input([agent({ installed_at: NOW - H })])) });
    expect(pending.pass).toBe(false);
    const good = evaluateGate({ ...base, canaryHealth: evaluateCanaryHealth(input([agent()])) });
    expect(good.pass).toBe(true);
  });
  it('does not add the check when no verdict is given', () => {
    expect(evaluateGate(base).checks.some((c) => c.name === 'canary_health')).toBe(false);
  });
});

describe('watchCanary', () => {
  let env: TestEnv;
  beforeEach(async () => {
    vi.useFakeTimers();
    setNow(T0);
    env = testEnv();
    await createTenant(env, 'demo', true);
  });
  afterEach(() => vi.useRealTimers());

  it('pauses the bad release (lane back to previous) and keeps blocking after recovery', async () => {
    const a = await enrollAgent(env, 'demo', { demo: true, version: '1.1.0' });
    const rows = (sql: string, ...p: unknown[]) => env.DB.prepare(sql).bind(...p).run();
    await rows('INSERT INTO agent_versions (agent_id, product, version, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?5)', a.id, 'dl-agent', '1.1.0', T0, T0);
    await rows('UPDATE agents SET last_seen_at = ?2, heartbeat_seconds = 15 WHERE id = ?1', a.id, T0);
    await rows(
      `INSERT INTO selftest_runs (id, agent_id, run_id, trigger, started_at, finished_at, agent_version, summary_json, checks_json, received_at)
       VALUES ('s1', ?1, 'r1', 'manual', ?2, ?2, '1.1.0', '{"pass":3,"fail":1}', '[]', ?2)`,
      a.id,
      T0 + 5,
    );
    const bad = await watchCanary(env, 'dl-agent', '1.1.0', '1.0.0', T0 + 60);
    expect(bad).toMatchObject({ verdict: 'rollback', rollbackTo: '1.0.0' });
    const pause = await env.DB.prepare('SELECT reason FROM release_pauses WHERE product = ?1 AND version = ?2').bind('dl-agent', '1.1.0').first<{ reason: string }>();
    expect(pause?.reason).toContain('auto-rollback: ');

    await rows(`UPDATE selftest_runs SET summary_json = '{"pass":4,"fail":0}'`);
    const later = await watchCanary(env, 'dl-agent', '1.1.0', '1.0.0', T0 + 5 * H);
    expect(later).toMatchObject({ verdict: 'rollback', promotable: false });
  });

  it('allows promotion after a healthy window and touches nothing', async () => {
    const a = await enrollAgent(env, 'demo', { demo: true, version: '1.1.0' });
    await env.DB.prepare('INSERT INTO agent_versions (agent_id, product, version, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?4)').bind(a.id, 'dl-agent', '1.1.0', T0).run();
    await env.DB.prepare('UPDATE agents SET last_seen_at = ?2 WHERE id = ?1').bind(a.id, T0 + 5 * H - 10).run();
    const r = await watchCanary(env, 'dl-agent', '1.1.0', '1.0.0', T0 + 5 * H);
    expect(r).toMatchObject({ verdict: 'healthy', promotable: true });
    const n = await env.DB.prepare('SELECT COUNT(*) AS n FROM release_pauses').first<{ n: number }>();
    expect(n?.n).toBe(0);
  });
});
