import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { admin, createTenant, enrollAgent, heartbeatBody, manifestText, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

const H = 3600;
let env: TestEnv;

interface Answer {
  commands: { id: string; kind: string }[];
  update_check: boolean;
}

async function beat(agent: TestAgent, version = '1.0.0'): Promise<Answer> {
  const res = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent, version));
  expect(res.status).toBe(200);
  return (await res.json()) as Answer;
}
const manifest = (agent: TestAgent, channel: 'canary' | 'stable'): Promise<Response> =>
  signed(env, agent, 'GET', `/v1/releases/dl-agent/${channel}`);
async function publish(version: string, at: number): Promise<void> {
  expect((await admin(env, 'PUT', `/admin/releases/dl-agent/${version}`, manifestText(version, 'canary', at))).status).toBe(200);
}
async function promote(version: string, at: number): Promise<void> {
  const body = { manifest: manifestText(version, 'stable', at), force: true, reason: 'test' };
  expect((await admin(env, 'POST', `/admin/releases/dl-agent/${version}/promote`, body)).status).toBe(200);
}
const pause = (version: string): Promise<Response> => admin(env, 'POST', `/admin/releases/dl-agent/${version}/pause`);
const resume = (version: string): Promise<Response> => admin(env, 'POST', `/admin/releases/dl-agent/${version}/resume`);

describe('kill switch', () => {
  let canary: TestAgent;
  let acme: TestAgent;
  let other: TestAgent;

  beforeEach(async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    setNow(T0);
    env = testEnv();
    canary = await enrollAgent(env, 'demo', { demo: true });
    acme = await enrollAgent(env, 'acme');
    other = await enrollAgent(env, 'other');
    await publish('1.0.0', T0 - 10 * H);
    await promote('1.0.0', T0 - 9 * H);
    await publish('1.0.1', T0 - 2 * H);
    await promote('1.0.1', T0 - 1 * H);
  });
  afterEach(() => vi.useRealTimers());

  describe('pause a release', () => {
    it('is not offered on canary or stable, and resume restores it', async () => {
      expect((await beat(canary)).update_check).toBe(true);
      expect((await beat(acme)).update_check).toBe(true);
      expect((await pause('1.0.1')).status).toBe(201);
      expect((await beat(canary)).update_check).toBe(false);
      expect((await beat(acme)).update_check).toBe(false);
      expect((await resume('1.0.1')).status).toBe(200);
      expect((await beat(canary)).update_check).toBe(true);
      expect((await beat(acme)).update_check).toBe(true);
    });

    it('the manifest falls back to the newest non-paused release', async () => {
      await pause('1.0.1');
      for (const channel of ['canary', 'stable'] as const) {
        const res = await manifest(acme, channel);
        expect(res.status).toBe(200);
        expect(JSON.parse(await res.text())).toMatchObject({ version: '1.0.0' });
      }
      // The fallback is not newer than an agent on 1.0.0: nothing is offered.
      expect((await beat(acme, '1.0.0')).update_check).toBe(false);
      await resume('1.0.1');
      expect(JSON.parse(await (await manifest(acme, 'stable')).text())).toMatchObject({ version: '1.0.1' });
    });

    it('a paused canary-only release is not served either', async () => {
      await publish('1.0.2', T0 - 60);
      await pause('1.0.2');
      expect((await beat(canary, '1.0.1')).update_check).toBe(false);
      expect(JSON.parse(await (await manifest(canary, 'canary')).text())).toMatchObject({ version: '1.0.1' });
    });

    it('shows paused in the gate and the release list; unknown or not-paused is 404', async () => {
      await pause('1.0.1');
      const gate = (await (await admin(env, 'GET', '/admin/releases/dl-agent/1.0.1/gate')).json()) as { paused: boolean };
      expect(gate.paused).toBe(true);
      const list = (await (await admin(env, 'GET', '/admin/releases')).json()) as { releases: { version: string; paused: boolean }[] };
      expect(list.releases.filter((r) => r.version === '1.0.1').every((r) => r.paused)).toBe(true);
      expect(list.releases.filter((r) => r.version === '1.0.0').every((r) => !r.paused)).toBe(true);
      expect((await pause('9.9.9')).status).toBe(404);
      await resume('1.0.1');
      expect((await resume('1.0.1')).status).toBe(404);
    });
  });

  describe('hold a tenant', () => {
    const hold = (tenant: string, body: Record<string, unknown> = {}): Promise<Response> =>
      admin(env, 'POST', `/admin/tenants/${tenant}/hold`, body);

    it('blocks update_check and the manifest for that tenant only; release restores', async () => {
      expect((await hold('acme', { reason: 'busy season' })).status).toBe(201);
      expect((await beat(acme)).update_check).toBe(false);
      expect((await manifest(acme, 'stable')).status).toBe(404);
      expect((await beat(other)).update_check).toBe(true);
      expect((await manifest(other, 'stable')).status).toBe(200);
      expect((await beat(canary)).update_check).toBe(true);
      expect((await admin(env, 'DELETE', '/admin/tenants/acme/hold', {})).status).toBe(200);
      expect((await beat(acme)).update_check).toBe(true);
      expect((await manifest(acme, 'stable')).status).toBe(200);
      expect((await admin(env, 'DELETE', '/admin/tenants/acme/hold', {})).status).toBe(404);
    });

    it('can cover one site or one product', async () => {
      expect((await hold('acme', { site_id: 'elsewhere' })).status).toBe(201);
      expect((await beat(acme)).update_check).toBe(true);
      expect((await hold('acme', { site_id: 'main' })).status).toBe(201);
      expect((await beat(acme)).update_check).toBe(false);
      await admin(env, 'DELETE', '/admin/tenants/acme/hold', { site_id: 'main' });
      await admin(env, 'DELETE', '/admin/tenants/acme/hold', { site_id: 'elsewhere' });
      expect((await hold('acme', { product: 'other-product' })).status).toBe(201);
      expect((await beat(acme)).update_check).toBe(true);
      const holds = (await (await admin(env, 'GET', '/admin/tenants/acme/hold')).json()) as { holds: { product: string }[] };
      expect(holds.holds).toHaveLength(1);
      expect((await hold('nobody')).status).toBe(404);
    });

    it('update_now overrides the hold: the manifest opens until the command is answered', async () => {
      await hold('acme');
      const sent = await admin(env, 'POST', '/admin/commands', { agent_id: acme.id, kind: 'update_now' });
      expect(sent.status).toBe(201);
      const answer = await beat(acme);
      expect(answer.update_check).toBe(false);
      expect(answer.commands.map((c) => c.kind)).toEqual(['update_now']);
      expect((await manifest(acme, 'stable')).status).toBe(200);
      const done = await signed(env, acme, 'POST', `/v1/commands/${answer.commands[0]?.id}/result`, { ok: true, output: 'ok', finished_at: T0 });
      expect(done.status).toBe(200);
      expect((await manifest(acme, 'stable')).status).toBe(404);
    });
  });
});
