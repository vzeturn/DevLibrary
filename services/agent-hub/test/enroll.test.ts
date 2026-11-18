import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { admin, createCode, createTenant, enrollRequest, newKeys, setNow, T0, testEnv, type TestEnv } from './helpers';

let env: TestEnv;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
});
afterEach(() => vi.useRealTimers());

async function body(code: string): Promise<Record<string, unknown>> {
  const { publicKey } = await newKeys();
  return { code, public_key: publicKey, hostname: 'FO-SERVER', os: 'windows', arch: 'x86_64', version: '1.0.0', modules: ['pda-sync'] };
}

describe('POST /v1/enroll', () => {
  it('enrols once and resolves tenant, site, channel and default config', async () => {
    await createTenant(env, 'demo-hotel', true);
    const code = await createCode(env, 'demo-hotel', 'hn-01');
    const res = await enrollRequest(env, await body(code.toLowerCase()));
    expect(res.status).toBe(200);
    const json = (await res.json()) as Record<string, unknown>;
    expect(json).toMatchObject({
      tenant: 'demo-hotel',
      site: 'hn-01',
      channel: 'canary',
      heartbeat_seconds: 60,
      config: { version: 0, body: { modules: {}, log_level: 'info' } },
    });
    expect(json.agent_id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);

    const again = await enrollRequest(env, await body(code));
    expect(again.status).toBe(403);
    expect(await again.json()).toEqual({ error: 'enroll_code_invalid' });
  });

  it('stores only the hash of the code', async () => {
    await createTenant(env, 'acme', false);
    const code = await createCode(env, 'acme');
    const rows = env.DB.raw.prepare('SELECT code_hash FROM enroll_codes').all() as { code_hash: string }[];
    expect(rows[0]?.code_hash).toMatch(/^[0-9a-f]{64}$/);
    expect(JSON.stringify(rows)).not.toContain(code);
    const listed = await (await admin(env, 'GET', '/admin/enroll-codes')).text();
    expect(listed).not.toContain('code_hash');
  });

  it('puts agents of normal tenants on stable', async () => {
    await createTenant(env, 'acme', false);
    const res = await enrollRequest(env, await body(await createCode(env, 'acme')));
    expect(((await res.json()) as { channel: string }).channel).toBe('stable');
  });

  it('refuses an expired code', async () => {
    await createTenant(env, 'acme', false);
    const code = await createCode(env, 'acme', 'main', 600);
    setNow(T0 + 600);
    const res = await enrollRequest(env, await body(code));
    expect(res.status).toBe(403);
  });

  it('refuses a revoked or unknown code', async () => {
    await createTenant(env, 'acme', false);
    const created = await admin(env, 'POST', '/admin/enroll-codes', { tenant_id: 'acme', site_id: 'main' });
    const { id, code } = (await created.json()) as { id: string; code: string };
    expect((await admin(env, 'DELETE', `/admin/enroll-codes/${id}`)).status).toBe(200);
    expect((await enrollRequest(env, await body(code))).status).toBe(403);
    expect((await enrollRequest(env, await body('AAAA-BBBB-CCCC-DDDD'))).status).toBe(403);
  });

  it('refuses a malformed public key', async () => {
    await createTenant(env, 'acme', false);
    const code = await createCode(env, 'acme');
    const res = await enrollRequest(env, { ...(await body(code)), public_key: 'AAAA' });
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: 'invalid_public_key' });
  });
});
