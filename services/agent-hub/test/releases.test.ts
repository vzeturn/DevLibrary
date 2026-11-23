import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { admin, enrollAgent, heartbeatBody, manifestText, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

const H = 3600;
let env: TestEnv;
let canary: TestAgent;

async function beat(version: string, extra: Record<string, unknown> = {}): Promise<void> {
  const res = await signed(env, canary, 'POST', '/v1/heartbeat', heartbeatBody(canary, version, extra));
  expect(res.status).toBe(200);
}
async function report(version: string, fingerprint: string, severity = 'error'): Promise<void> {
  const r = { fingerprint, severity, module: 'pda-sync', code: 'x', message: 'm', first_seen: T0, last_seen: T0, count: 1, agent_version: version };
  expect((await signed(env, canary, 'POST', '/v1/reports', { reports: [r] })).status).toBe(200);
}
const promote = (body: Record<string, unknown>): Promise<Response> =>
  admin(env, 'POST', '/admin/releases/dl-agent/1.0.1/promote', { manifest: manifestText('1.0.1', 'stable', T0 + 3 * H), ...body });

/** 1.0.0 is stable and seen with fingerprint `c0de`; 1.0.1 (a patch: 2 h soak) is on canary with one demo agent. */
beforeEach(async () => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
  canary = await enrollAgent(env, 'demo', { demo: true });
  await admin(env, 'PUT', '/admin/releases/dl-agent/1.0.0', manifestText('1.0.0', 'canary', T0 - 10 * H));
  await admin(env, 'POST', '/admin/releases/dl-agent/1.0.0/promote', { manifest: manifestText('1.0.0', 'stable', T0 - 9 * H), force: true, reason: 'bootstrap' });
  await report('1.0.0', 'c0de');
  expect((await admin(env, 'PUT', '/admin/releases/dl-agent/1.0.1', manifestText('1.0.1', 'canary', T0))).status).toBe(200);
});
afterEach(() => vi.useRealTimers());

async function gate(): Promise<{ pass: boolean; previous_version: string | null; checks: { name: string; ok: boolean }[] }> {
  const res = await admin(env, 'GET', '/admin/releases/dl-agent/1.0.1/gate');
  expect(res.status).toBe(200);
  return (await res.json()) as { pass: boolean; previous_version: string | null; checks: { name: string; ok: boolean }[] };
}
const failing = async (): Promise<string[]> => (await gate()).checks.filter((c) => !c.ok).map((c) => c.name);

async function soak(): Promise<void> {
  await beat('1.0.1');
  setNow(T0 + 2 * H);
  await beat('1.0.1');
}

describe('releases', () => {
  it('serves the stored manifest verbatim per channel and 404 without one', async () => {
    const res = await signed(env, canary, 'GET', '/v1/releases/dl-agent/canary');
    expect(res.status).toBe(200);
    expect(await res.text()).toBe(manifestText('1.0.1', 'canary', T0));
    expect(await (await signed(env, canary, 'GET', '/v1/releases/dl-agent/stable')).text()).toBe(manifestText('1.0.0', 'stable', T0 - 9 * H));
    expect((await signed(env, canary, 'GET', '/v1/releases/other/stable')).status).toBe(404);
  });

  it('accepts a manifest without a minimum version (min_version null, as dl-agent signs it)', async () => {
    const text = manifestText('1.2.0', 'canary', T0 + 1).replace('"min_version":"1.0.0"', '"min_version":null');
    expect((await admin(env, 'PUT', '/admin/releases/dl-agent/1.2.0', text)).status).toBe(200);
    const served = await signed(env, canary, 'GET', '/v1/releases/dl-agent/canary');
    expect(await served.text()).toBe(text);
  });

  it('rejects a canary upload with the wrong shape, channel or version', async () => {
    expect((await admin(env, 'PUT', '/admin/releases/dl-agent/1.2.0', '{"product":"dl-agent"}')).status).toBe(400);
    expect((await admin(env, 'PUT', '/admin/releases/dl-agent/1.2.0', manifestText('1.2.0', 'stable', T0))).status).toBe(400);
    const mismatch = await admin(env, 'PUT', '/admin/releases/dl-agent/1.2.0', manifestText('1.3.0', 'canary', T0));
    expect(await mismatch.json()).toMatchObject({ error: 'manifest_mismatch' });
  });

  it('gate: fails min_agents and soak until a canary agent has run the version 2 h', async () => {
    expect(await failing()).toEqual(['min_agents', 'soak']);
    await beat('1.0.1');
    expect(await failing()).toEqual(['soak']);
    setNow(T0 + 2 * H);
    await beat('1.0.1');
    const g = await gate();
    expect(g.pass).toBe(true);
    expect(g.previous_version).toBe('1.0.0');
  });

  it('gate: stable agents do not count', async () => {
    await admin(env, 'PUT', `/admin/agents/${canary.id}/channel`, { channel: 'stable' });
    await soak();
    expect(await failing()).toEqual(['min_agents', 'soak']);
  });

  it('gate: fails modules_running on a degraded module', async () => {
    await soak();
    await beat('1.0.1', { modules: [{ name: 'pda-sync', state: 'degraded' }] });
    expect(await failing()).toEqual(['modules_running']);
  });

  it('gate: fails no_new_errors only for fingerprints new on this version', async () => {
    await soak();
    await report('1.0.1', 'c0de');
    await report('1.0.1', 'aaaa', 'warning');
    expect(await failing()).toEqual([]);
    await report('1.0.1', 'bbbb', 'critical');
    expect(await failing()).toEqual(['no_new_errors']);
  });

  it('gate: fails no_rollback when an agent rolled the version back', async () => {
    await soak();
    const other = await enrollAgent(env, 'demo2', { demo: true });
    const hb = heartbeatBody(other, '1.0.0', { update: { state: 'rolled_back', version: '1.0.1' } });
    expect((await signed(env, other, 'POST', '/v1/heartbeat', hb)).status).toBe(200);
    expect(await failing()).toEqual(['no_rollback']);
  });

  it('heartbeat: a deferred update is accepted, never a 400 (RT-0818)', async () => {
    const other = await enrollAgent(env, 'demo3', { demo: true });
    const hb = heartbeatBody(other, '1.0.0', { update: { state: 'deferred', version: 'x'.repeat(200), detail: 'outside window' } });
    expect((await signed(env, other, 'POST', '/v1/heartbeat', hb)).status).toBe(200);
  });

  it('promote: 409 with failing checks while the gate fails', async () => {
    const res = await promote({});
    expect(res.status).toBe(409);
    const body = (await res.json()) as { error: string; checks: { name: string }[] };
    expect(body.error).toBe('gate_failed');
    expect(body.checks.map((c) => c.name)).toEqual(['min_agents', 'soak']);
    expect((await signed(env, canary, 'GET', '/v1/releases/dl-agent/stable')).headers.get('content-type')).toContain('json');
    expect(await (await signed(env, canary, 'GET', '/v1/releases/dl-agent/stable')).text()).toContain('"1.0.0"');
  });

  it('promote: stores the separately signed stable manifest when the gate passes', async () => {
    await soak();
    const res = await promote({});
    expect(res.status).toBe(200);
    expect(await res.json()).toMatchObject({ promoted: true, forced: false });
    expect(await (await signed(env, canary, 'GET', '/v1/releases/dl-agent/stable')).text()).toBe(manifestText('1.0.1', 'stable', T0 + 3 * H));
  });

  it('promote: force needs a reason and is recorded', async () => {
    expect((await promote({ force: true })).status).toBe(400);
    const res = await promote({ force: true, reason: 'hotfix for a site outage' });
    expect(res.status).toBe(200);
    expect(await res.json()).toMatchObject({ promoted: true, forced: true });
    const g = (await (await admin(env, 'GET', '/admin/releases/dl-agent/1.0.1/gate')).json()) as { promotions: Record<string, unknown>[] };
    expect(g.promotions[0]).toMatchObject({ forced: 1, reason: 'hotfix for a site outage' });
  });

  it('promote: rejects a manifest for another version or the canary channel', async () => {
    await soak();
    const wrongVersion = await admin(env, 'POST', '/admin/releases/dl-agent/1.0.1/promote', { manifest: manifestText('1.2.0', 'stable', T0) });
    expect(wrongVersion.status).toBe(400);
    const wrongChannel = await admin(env, 'POST', '/admin/releases/dl-agent/1.0.1/promote', { manifest: manifestText('1.0.1', 'canary', T0) });
    expect(wrongChannel.status).toBe(400);
    const unknown = await admin(env, 'POST', '/admin/releases/dl-agent/9.9.9/promote', { manifest: manifestText('9.9.9', 'stable', T0) });
    expect(unknown.status).toBe(404);
  });
});

describe('admin and artifacts', () => {
  it('requires the admin bearer token', async () => {
    const { app } = await import('../src/index');
    expect((await app.request('https://hub.test/admin/agents', {}, env)).status).toBe(401);
    const wrong = await app.request('https://hub.test/admin/agents', { headers: { Authorization: 'Bearer nope' } }, env);
    expect(wrong.status).toBe(401);
  });

  it('renders the status page with escaped values', async () => {
    await admin(env, 'PUT', '/admin/tenants/x', { name: 'X', demo: false });
    await beat('1.0.1', { hostname: '<b>FO</b>' });
    const res = await admin(env, 'GET', '/admin');
    expect(res.status).toBe(200);
    const html = await res.text();
    expect(html).toContain('Trực tuyến');
    expect(html).toContain('&lt;b&gt;FO&lt;/b&gt;');
    expect(html).toContain('Chưa đạt');
    expect(html).toContain('Đã lên stable');
  });

  it('streams artifacts from R2 to signed agents only', async () => {
    env.ARTIFACTS.store.set('dl-agent/1.0.1/a.exe', new Uint8Array([1, 2, 3]));
    const res = await signed(env, canary, 'GET', '/v1/artifacts/dl-agent/1.0.1/a.exe');
    expect(res.status).toBe(200);
    expect(new Uint8Array(await res.arrayBuffer())).toEqual(new Uint8Array([1, 2, 3]));
    expect((await signed(env, canary, 'GET', '/v1/artifacts/missing')).status).toBe(404);
    const { app } = await import('../src/index');
    expect((await app.request('https://hub.test/v1/artifacts/dl-agent/1.0.1/a.exe', {}, env)).status).toBe(401);
  });
});
