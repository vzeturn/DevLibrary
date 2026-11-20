import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { admin, createCode, createTenant, enrollAgent, heartbeatBody, newKeys, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

let env: TestEnv;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
});
afterEach(() => vi.useRealTimers());

async function productCode(product: string, externalId: string): Promise<string> {
  const key = (await (await admin(env, 'POST', '/admin/product-keys', { product, label: product })).json()) as { key: string };
  const res = await app.request(
    `https://hub.test/products/${product}/enroll-codes`,
    { method: 'POST', headers: { Authorization: `Bearer ${key.key}`, 'Content-Type': 'application/json' }, body: JSON.stringify({ external_id: externalId, name: 'Mui Ne Bay' }) },
    env,
  );
  expect(res.status).toBe(201);
  return ((await res.json()) as { code: string }).code;
}

async function attach(agent: TestAgent, code: string): Promise<Response> {
  return signed(env, agent, 'POST', '/v1/attach', { code });
}

async function links(query = ''): Promise<Record<string, unknown>[]> {
  return ((await (await admin(env, 'GET', `/admin/product-links${query}`)).json()) as { links: Record<string, unknown>[] }).links;
}

function configVersion(agent: TestAgent): number {
  return (env.DB.raw.prepare('SELECT version FROM configs WHERE agent_id = ?').get(agent.id) as { version: number }).version;
}

/** An agent enrolled for Check-In (`arrival`) with its settings already in its config. */
async function arrivalAgent(): Promise<TestAgent> {
  const { publicKey, keys } = await newKeys();
  const code = await productCode('arrival', 'mnb');
  const res = await app.request('https://hub.test/v1/enroll', {
    method: 'POST',
    body: JSON.stringify({ code, public_key: publicKey, hostname: 'MNB-FO', os: 'windows', arch: 'x86_64', version: '1.4.0', modules: ['arrival', 'pda-sync'] }),
  }, env);
  expect(res.status).toBe(200);
  const { agent_id } = (await res.json()) as { agent_id: string };
  const agent = { id: agent_id, keys, publicKey };
  const cfg = await admin(env, 'PUT', `/admin/agents/${agent_id}/config`, { body: { modules: { arrival: { enabled: true, poll_seconds: 30 } }, log_level: 'debug' } });
  expect(cfg.status).toBe(200);
  return agent;
}

describe('POST /v1/attach', () => {
  it('adds a second product to the agent already installed: same identity, both modules on, link recorded', async () => {
    const agent = await arrivalAgent();
    setNow(T0 + 3600);
    const code = await productCode('pda-sync', 'mnb-pos');
    const res = await attach(agent, code);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({
      agent_id: agent.id,
      tenant: 'arrival-mnb',
      site: 'main',
      product: 'pda-sync',
      product_tenant: 'pda-sync-mnb-pos',
      modules: ['pda-sync'],
      config: { version: 3, body: { modules: { arrival: { enabled: true, poll_seconds: 30 }, 'pda-sync': { enabled: true } }, log_level: 'debug' } },
    });
    // The next heartbeat hands the merged config to the agent.
    const hb = (await (await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent, '1.4.0', { config_version: 2 }))).json()) as { config?: { version: number } };
    expect(hb.config?.version).toBe(3);
    // Still one agent, enrolled once.
    const agents = (await (await admin(env, 'GET', '/admin/agents')).json()) as { agents: { id: string }[] };
    expect(agents.agents.map((a) => a.id)).toEqual([agent.id]);
    expect((await links()).map((l) => [l.product, l.product_tenant, l.agent_tenant])).toEqual([
      ['pda-sync', 'pda-sync-mnb-pos', 'arrival-mnb'],
      ['arrival', 'arrival-mnb', 'arrival-mnb'],
    ]);
    expect(await links('?tenant_id=pda-sync-mnb-pos')).toHaveLength(1);
    expect(await links('?tenant_id=arrival-mnb')).toHaveLength(2);
    expect(await links(`?agent_id=${agent.id}&product=arrival`)).toHaveLength(1);
  });

  it('spends the code once: a second attach or an enrolment with it is refused', async () => {
    const agent = await arrivalAgent();
    const code = await productCode('einvoice', 'mnb');
    expect((await attach(agent, code)).status).toBe(200);
    const again = await attach(agent, code);
    expect(again.status).toBe(403);
    expect(await again.json()).toMatchObject({ error: 'enroll_code_invalid' });
    expect(configVersion(agent)).toBe(3);
  });

  it('refuses an unknown, expired or revoked code without touching the config', async () => {
    const agent = await arrivalAgent();
    expect((await attach(agent, 'NOPE-NOPE')).status).toBe(403);
    await createTenant(env, 'arrival-mnb', false);
    const short = await createCode(env, 'arrival-mnb', 'main', 60);
    setNow(T0 + 61);
    expect((await attach(agent, short)).status).toBe(403);
    expect(configVersion(agent)).toBe(2);
  });

  it('takes an admin code only inside the agent tenant', async () => {
    const agent = await arrivalAgent();
    await createTenant(env, 'other', false);
    const foreign = await attach(agent, await createCode(env, 'other'));
    expect(foreign.status).toBe(409);
    expect(await foreign.json()).toMatchObject({ error: 'tenant_mismatch' });
    const own = await attach(agent, await createCode(env, 'arrival-mnb'));
    expect(own.status).toBe(200);
    expect(await own.json()).toMatchObject({ product: null, modules: [], config: { version: 3 } });
    expect(await links()).toHaveLength(1);
  });

  it('needs a signed agent: the code alone does not attach', async () => {
    const code = await productCode('pda-sync', 'x');
    const res = await app.request('https://hub.test/v1/attach', { method: 'POST', body: JSON.stringify({ code }) }, env);
    expect(res.status).toBe(401);
  });

  it('starts from the default config for an agent that never had one', async () => {
    const agent = await enrollAgent(env, 'plain');
    const res = await attach(agent, await productCode('hotellink', 'h1'));
    expect(await res.json()).toMatchObject({ config: { version: 1, body: { modules: { hotellink: { enabled: true } }, log_level: 'info' } } });
  });
});
