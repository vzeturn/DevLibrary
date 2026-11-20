import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { deepMerge, mergeModules } from '../src/services/attach';
import { admin, enrollRequest, newKeys, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

let env: TestEnv;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
});
afterEach(() => vi.useRealTimers());

interface Answer {
  config: { version: number; body: Record<string, unknown> };
  attached: string[];
}

async function productKey(product: string): Promise<string> {
  const res = await admin(env, 'POST', '/admin/product-keys', { product, label: product });
  expect(res.status).toBe(201);
  return ((await res.json()) as { key: string }).key;
}

async function mint(product: string, key: string, externalId: string, ttl = 3600): Promise<string> {
  const res = await app.request(
    `https://hub.test/products/${product}/enroll-codes`,
    {
      method: 'POST',
      headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ external_id: externalId, name: externalId, ttl_seconds: ttl }),
    },
    env,
  );
  expect(res.status).toBe(201);
  return ((await res.json()) as { code: string }).code;
}

/** Enrols a machine for `arrival` (Check-In) the way a product install line does. */
async function enrolArrival(): Promise<TestAgent & { code: string }> {
  const code = await mint('arrival', await productKey('arrival'), 'muinebay');
  const { keys, publicKey } = await newKeys();
  const res = await enrollRequest(env, { code, public_key: publicKey, hostname: 'FO', os: 'windows', arch: 'x86_64', version: '1.4.0', modules: ['arrival'] });
  expect(res.status).toBe(200);
  return { id: ((await res.json()) as { agent_id: string }).agent_id, keys, publicKey, code };
}

function attach(agent: TestAgent, code: string, opt = {}): Promise<Response> {
  return signed(env, agent, 'POST', '/v1/attach', { code }, opt);
}

function links(): Record<string, unknown>[] {
  return env.DB.raw.prepare('SELECT * FROM product_links ORDER BY product').all() as Record<string, unknown>[];
}

describe('POST /v1/attach', () => {
  it('adds the second product and keeps the first module, the identity and the tenant', async () => {
    const agent = await enrolArrival();
    expect((await admin(env, 'PUT', `/admin/agents/${agent.id}/config`, {
      body: { modules: { arrival: { enabled: true, poll: 5 } }, log_level: 'debug', sql: { max: 2 } },
    })).status).toBe(200);
    const code = await mint('pda-sync', await productKey('pda-sync'), 'mnb-pos');

    const res = await attach(agent, ` ${code.toLowerCase()} `);
    expect(res.status).toBe(200);
    const answer = (await res.json()) as Answer;
    expect(answer).toEqual({
      attached: ['pda-sync'],
      config: {
        version: 3,
        body: { modules: { arrival: { enabled: true, poll: 5 }, 'pda-sync': { enabled: true } }, log_level: 'debug', sql: { max: 2 } },
      },
    });

    // The next heartbeat of an agent still on version 2 receives the merged config.
    const hb = await signed(env, agent, 'POST', '/v1/heartbeat', {
      agent_id: agent.id, version: '1.4.0', channel: 'stable', target: 'windows-x86_64', hostname: 'FO',
      started_at: T0, uptime_seconds: 1, config_version: 2, update: { state: 'idle', version: '1.4.0' }, modules: [],
    });
    expect(((await hb.json()) as { config: unknown }).config).toEqual(answer.config);

    expect(links()).toEqual([
      { product: 'arrival', external_id: 'muinebay', tenant_id: 'arrival-muinebay', agent_id: agent.id, site_id: 'main', created_at: T0 },
      { product: 'pda-sync', external_id: 'mnb-pos', tenant_id: 'arrival-muinebay', agent_id: agent.id, site_id: 'main', created_at: T0 },
    ]);
    const view = (await (await admin(env, 'GET', `/admin/agents/${agent.id}`)).json()) as { agent: Record<string, unknown> };
    expect(view.agent).toMatchObject({
      id: agent.id,
      tenant_id: 'arrival-muinebay',
      products: [
        { product: 'arrival', external_id: 'muinebay', created_at: T0 },
        { product: 'pda-sync', external_id: 'mnb-pos', created_at: T0 },
      ],
    });
    const list = (await (await admin(env, 'GET', '/admin/agents')).json()) as { agents: { products: unknown[] }[] };
    expect(list.agents[0]?.products).toHaveLength(2);
    expect((await admin(env, 'GET', '/admin/agents/nope')).status).toBe(404);

    const used = env.DB.raw.prepare('SELECT used_at, used_by_agent FROM enroll_codes WHERE minted_by = ?').get('pda-sync');
    expect(used).toEqual({ used_at: T0, used_by_agent: agent.id });
    const audit = env.DB.raw.prepare("SELECT actor, path, body, status FROM admin_audit WHERE path = '/v1/attach'").all() as Record<string, unknown>[];
    expect(audit).toHaveLength(1);
    expect(audit[0]).toMatchObject({ actor: `agent:${agent.id}`, status: 200 });
    expect(String(audit[0]?.body)).not.toContain(code);
  });

  it('is idempotent: the same code again re-merges without a new version, a new code refreshes', async () => {
    const agent = await enrolArrival();
    const key = await productKey('pda-sync');
    const code = await mint('pda-sync', key, 'mnb-pos');
    const first = (await (await attach(agent, code)).json()) as Answer;
    expect(first.config.version).toBe(2);

    const again = await attach(agent, code);
    expect(again.status).toBe(200);
    expect(await again.json()).toEqual(first);

    // The first product's install line run again sends the enrolment code: nothing changes.
    const rerun = await attach(agent, agent.code);
    expect(rerun.status).toBe(200);
    expect(await rerun.json()).toEqual({ attached: ['arrival'], config: first.config });

    // A code minted while the link exists still mints; attaching it changes nothing but the code.
    setNow(T0 + 60);
    const second = await mint('pda-sync', key, 'mnb-pos');
    const res = await attach(agent, second);
    expect(res.status).toBe(200);
    expect(((await res.json()) as Answer).config).toEqual(first.config);
    expect(links().find((l) => l.product === 'pda-sync')).toMatchObject({ agent_id: agent.id, created_at: T0 });
  });

  it('re-enables a module the code enables after an admin turned it off', async () => {
    const agent = await enrolArrival();
    const code = await mint('pda-sync', await productKey('pda-sync'), 'mnb-pos');
    await attach(agent, code);
    await admin(env, 'PUT', `/admin/agents/${agent.id}/config`, {
      body: { modules: { arrival: { enabled: true }, 'pda-sync': { enabled: false, rvc: 3 } } },
    });
    const answer = (await (await attach(agent, code)).json()) as Answer;
    expect(answer.config).toEqual({ version: 4, body: { modules: { arrival: { enabled: true }, 'pda-sync': { enabled: true, rvc: 3 } } } });
  });

  it('refuses used, expired, revoked and unknown codes without touching the config', async () => {
    const agent = await enrolArrival();
    const other = await enrolArrival();
    const key = await productKey('pda-sync');

    const used = await mint('pda-sync', key, 'a');
    expect((await attach(other, used)).status).toBe(200);
    const taken = await attach(agent, used);
    expect(taken.status).toBe(403);
    expect(await taken.json()).toEqual({ error: 'enroll_code_invalid' });

    const expired = await mint('pda-sync', key, 'b', 60);
    setNow(T0 + 61);
    expect((await attach(agent, expired)).status).toBe(403);

    const revoked = await mint('pda-sync', key, 'c');
    const id = (env.DB.raw.prepare("SELECT id FROM enroll_codes WHERE tenant_id = 'pda-sync-c'").get() as { id: string }).id;
    expect((await admin(env, 'DELETE', `/admin/enroll-codes/${id}`)).status).toBe(200);
    expect((await attach(agent, revoked)).status).toBe(403);

    expect((await attach(agent, 'NOPE-NOPE')).status).toBe(403);
    expect((await signed(env, agent, 'POST', '/v1/attach', { code: 'x' })).status).toBe(400);

    // Both machines enrolled for arrival/muinebay: the newer enrolment holds that link.
    expect(links().map((l) => [l.product, l.external_id, l.agent_id])).toEqual([
      ['arrival', 'muinebay', other.id],
      ['pda-sync', 'a', other.id],
    ]);
    const row = env.DB.raw.prepare('SELECT version FROM configs WHERE agent_id = ?').get(agent.id);
    expect(row).toEqual({ version: 1 });
  });

  it('refuses an admin code without modules and leaves it unused', async () => {
    const agent = await enrolArrival();
    await admin(env, 'PUT', '/admin/tenants/acme', { name: 'Acme', demo: false });
    const res = await admin(env, 'POST', '/admin/enroll-codes', { tenant_id: 'acme', site_id: 'main' });
    const { code, id } = (await res.json()) as { code: string; id: string };
    const answer = await attach(agent, code);
    expect(answer.status).toBe(400);
    expect(await answer.json()).toEqual({ error: 'code_has_no_modules' });
    expect(env.DB.raw.prepare('SELECT used_at FROM enroll_codes WHERE id = ?').get(id)).toEqual({ used_at: null });
  });

  it('merges an admin code with modules and records no product link', async () => {
    const agent = await enrolArrival();
    await admin(env, 'PUT', '/admin/tenants/acme', { name: 'Acme', demo: false });
    const res = await admin(env, 'POST', '/admin/enroll-codes', {
      tenant_id: 'acme', site_id: 'main', config: { modules: { speaker: { enabled: true, rvcList: [1] } }, log_level: 'debug' },
    });
    const answer = (await (await attach(agent, ((await res.json()) as { code: string }).code)).json()) as Answer;
    expect(answer).toEqual({
      attached: ['speaker'],
      config: { version: 2, body: { modules: { arrival: { enabled: true }, speaker: { enabled: true, rvcList: [1] } }, log_level: 'info' } },
    });
    expect(links().map((l) => l.product)).toEqual(['arrival']);
  });

  it('refuses unsigned, wrongly signed and revoked agents', async () => {
    const agent = await enrolArrival();
    const code = await mint('pda-sync', await productKey('pda-sync'), 'mnb-pos');
    const plain = await app.request('https://hub.test/v1/attach', { method: 'POST', body: JSON.stringify({ code }) }, env);
    expect(plain.status).toBe(401);
    const { keys } = await newKeys();
    expect((await attach(agent, code, { signWith: keys })).status).toBe(401);
    expect((await signed(env, agent, 'POST', '/v1/attach', { code }, { sendBody: JSON.stringify({ code: 'OTHER-CODE' }) })).status).toBe(401);
    expect((await admin(env, 'POST', `/admin/agents/${agent.id}/revoke`)).status).toBe(200);
    expect((await attach(agent, code)).status).toBe(401);
    expect(env.DB.raw.prepare("SELECT used_at FROM enroll_codes WHERE minted_by = 'pda-sync'").get()).toEqual({ used_at: null });
  });

  it('lets one of two agents racing for one code win', async () => {
    const a = await enrolArrival();
    const b = await enrolArrival();
    const code = await mint('pda-sync', await productKey('pda-sync'), 'mnb-pos');
    const statuses = (await Promise.all([attach(a, code), attach(b, code)])).map((r) => r.status).sort();
    expect(statuses).toEqual([200, 403]);
    const winner = env.DB.raw.prepare("SELECT used_by_agent FROM enroll_codes WHERE minted_by = 'pda-sync'").get() as { used_by_agent: string };
    const versions = env.DB.raw.prepare('SELECT agent_id, version FROM configs ORDER BY version').all() as { agent_id: string; version: number }[];
    expect(versions.find((v) => v.agent_id === winner.used_by_agent)?.version).toBe(2);
    expect(versions.find((v) => v.agent_id !== winner.used_by_agent)?.version).toBe(1);
    expect(links().find((l) => l.product === 'pda-sync')).toMatchObject({ agent_id: winner.used_by_agent });
  });

  it('bumps the version once when one agent sends the same code twice at once', async () => {
    const agent = await enrolArrival();
    const code = await mint('pda-sync', await productKey('pda-sync'), 'mnb-pos');
    const answers = await Promise.all([attach(agent, code), attach(agent, code)]);
    expect(answers.map((r) => r.status)).toEqual([200, 200]);
    expect(env.DB.raw.prepare('SELECT version FROM configs WHERE agent_id = ?').get(agent.id)).toEqual({ version: 2 });
  });

  it('does not overwrite a config an admin changed between the read and the write', async () => {
    const agent = await enrolArrival();
    const code = await mint('pda-sync', await productKey('pda-sync'), 'mnb-pos');
    const [res] = await Promise.all([
      attach(agent, code),
      admin(env, 'PUT', `/admin/agents/${agent.id}/config`, { body: { modules: { arrival: { enabled: true, poll: 9 } } } }),
    ]);
    expect(res?.status).toBe(200);
    const row = env.DB.raw.prepare('SELECT body FROM configs WHERE agent_id = ?').get(agent.id) as { body: string };
    const body = JSON.parse(row.body) as { modules: Record<string, unknown> };
    expect(body.modules['pda-sync']).toEqual({ enabled: true });
    expect(body.modules.arrival).toEqual({ enabled: true, poll: 9 });
  });
});

describe('product links on enrolment', () => {
  it('enrolment records the product of a minted code under the agent tenant', async () => {
    const agent = await enrolArrival();
    expect(links()).toEqual([
      { product: 'arrival', external_id: 'muinebay', tenant_id: 'arrival-muinebay', agent_id: agent.id, site_id: 'main', created_at: T0 },
    ]);
  });

  it('the migration backfill keeps the newest live agent and skips admin codes', async () => {
    const live = await enrolArrival();
    setNow(T0 + 10);
    const revoked = await enrolArrival();
    expect((await admin(env, 'POST', `/admin/agents/${revoked.id}/revoke`)).status).toBe(200);
    await admin(env, 'PUT', '/admin/tenants/acme', { name: 'Acme', demo: false });
    const plain = ((await (await admin(env, 'POST', '/admin/enroll-codes', { tenant_id: 'acme', site_id: 'main' })).json()) as { code: string }).code;
    const { publicKey } = await newKeys();
    await enrollRequest(env, { code: plain, public_key: publicKey, hostname: 'X', os: 'windows', arch: 'x86_64', version: '1.4.0', modules: [] });

    const migration = readFileSync(join(fileURLToPath(import.meta.url), '..', '..', 'migrations', '0008_product_links.sql'), 'utf8');
    env.DB.raw.exec('DELETE FROM product_links');
    env.DB.raw.exec(migration.slice(migration.indexOf('INSERT INTO product_links')));
    expect(links()).toEqual([
      { product: 'arrival', external_id: 'muinebay', tenant_id: 'arrival-muinebay', agent_id: live.id, site_id: 'main', created_at: T0 },
    ]);
  });
});

describe('mergeModules', () => {
  it('lets the code win only for the keys it sets', () => {
    const body = { modules: { a: { enabled: false, x: { y: 1, z: 2 } }, b: { enabled: true } }, log_level: 'debug' };
    expect(mergeModules(body, { a: { enabled: true, x: { y: 5 } }, c: { enabled: true } })).toEqual({
      modules: { a: { enabled: true, x: { y: 5, z: 2 } }, b: { enabled: true }, c: { enabled: true } },
      log_level: 'debug',
    });
    expect(mergeModules(null, { a: { enabled: true } })).toEqual({ modules: { a: { enabled: true } } });
    expect(mergeModules({ modules: { a: 'bad' } }, { a: { enabled: true } })).toEqual({ modules: { a: { enabled: true } } });
  });

  it('ignores __proto__ keys', () => {
    const patch = JSON.parse('{"__proto__": {"polluted": true}, "a": 1}') as Record<string, unknown>;
    const out = deepMerge({}, patch);
    expect(out).toEqual({ a: 1 });
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();
  });
});
