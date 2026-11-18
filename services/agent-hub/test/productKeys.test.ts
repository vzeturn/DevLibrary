import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { admin, createCode, createTenant, enrollRequest, newKeys, setNow, T0, testEnv, type TestEnv } from './helpers';

let env: TestEnv;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
});
afterEach(() => vi.useRealTimers());

async function newKey(product: string): Promise<{ id: string; key: string }> {
  const res = await admin(env, 'POST', '/admin/product-keys', { product, label: `${product} platform` });
  expect(res.status).toBe(201);
  return (await res.json()) as { id: string; key: string };
}

function mint(product: string, key: string | null, body: unknown): Promise<Response> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (key) headers.Authorization = `Bearer ${key}`;
  return Promise.resolve(app.request(`https://hub.test/products/${product}/enroll-codes`, { method: 'POST', headers, body: JSON.stringify(body) }, env));
}

async function enrollWith(code: string): Promise<Record<string, unknown>> {
  const { publicKey } = await newKeys();
  const res = await enrollRequest(env, { code, public_key: publicKey, hostname: 'FO', os: 'windows', arch: 'x86_64', version: '1.1.0', modules: ['einvoice'] });
  expect(res.status).toBe(200);
  return (await res.json()) as Record<string, unknown>;
}

describe('product keys', () => {
  it('mints a code for the product tenant whose agent starts with the module enabled', async () => {
    const { key } = await newKey('einvoice');
    expect(key).toMatch(/^dlpk_[A-Za-z0-9_-]{43}$/);
    const res = await mint('einvoice', key, { external_id: 'tenant-uuid-1', name: 'Hotel A', demo: true });
    expect(res.status).toBe(201);
    const created = (await res.json()) as { code: string; tenant_id: string; site_id: string; install_command: string };
    expect(created).toMatchObject({ tenant_id: 'einvoice-tenant-uuid-1', site_id: 'main' });
    expect(created.install_command).toContain(`-Code '${created.code}'`);

    const enrolled = await enrollWith(created.code);
    expect(enrolled).toMatchObject({
      tenant: 'einvoice-tenant-uuid-1',
      channel: 'canary',
      config: { version: 1, body: { modules: { einvoice: { enabled: true } }, log_level: 'info' } },
    });
    const codes = (await (await admin(env, 'GET', '/admin/enroll-codes')).json()) as { codes: Record<string, unknown>[] };
    expect(codes.codes[0]).toMatchObject({ minted_by: 'einvoice' });
    expect(JSON.stringify(codes)).not.toContain('"config"');
  });

  it('keeps an existing tenant as the admin set it', async () => {
    const { key } = await newKey('einvoice');
    await createTenant(env, 'einvoice-t1', false);
    const res = await mint('einvoice', key, { external_id: 't1', name: 'Renamed', demo: true });
    expect(res.status).toBe(201);
    const enrolled = await enrollWith(((await res.json()) as { code: string }).code);
    expect(enrolled.channel).toBe('stable');
  });

  it('refuses missing, wrong, other-product and revoked keys', async () => {
    const einvoice = await newKey('einvoice');
    const other = await newKey('pms');
    const body = { external_id: 't1', name: 'T1' };
    expect((await mint('einvoice', null, body)).status).toBe(401);
    expect((await mint('einvoice', 'dlpk_wrong', body)).status).toBe(401);
    expect((await mint('einvoice', 'test-admin-token', body)).status).toBe(401);
    expect((await mint('einvoice', other.key, body)).status).toBe(401);
    expect((await admin(env, 'DELETE', `/admin/product-keys/${einvoice.id}`)).status).toBe(200);
    expect((await mint('einvoice', einvoice.key, body)).status).toBe(401);
    expect((await admin(env, 'DELETE', `/admin/product-keys/${einvoice.id}`)).status).toBe(404);
  });

  it('stores only the hash and lists keys without it', async () => {
    const { key } = await newKey('einvoice');
    await mint('einvoice', key, { external_id: 't1', name: 'T1' });
    const rows = env.DB.raw.prepare('SELECT key_hash, last_used FROM product_keys').all() as { key_hash: string; last_used: number }[];
    expect(rows[0]?.key_hash).toMatch(/^[0-9a-f]{64}$/);
    expect(rows[0]?.last_used).toBe(T0);
    const listed = await (await admin(env, 'GET', '/admin/product-keys')).text();
    expect(listed).not.toContain(key);
    expect(listed).not.toContain('key_hash');
  });

  it('validates the product name and the request body', async () => {
    expect((await admin(env, 'POST', '/admin/product-keys', { product: 'E-Invoice', label: 'x' })).status).toBe(400);
    const { key } = await newKey('einvoice');
    expect((await mint('einvoice', key, { external_id: 'bad id', name: 'T' })).status).toBe(400);
    expect((await mint('einvoice', key, { external_id: 'x'.repeat(60), name: 'T' })).status).toBe(400);
  });

  it('lets an admin code carry an initial config', async () => {
    await createTenant(env, 'acme', false);
    const res = await admin(env, 'POST', '/admin/enroll-codes', { tenant_id: 'acme', site_id: 'main', config: { modules: { sql: { enabled: true } } } });
    const enrolled = await enrollWith(((await res.json()) as { code: string }).code);
    expect(enrolled.config).toEqual({ version: 1, body: { modules: { sql: { enabled: true } } } });
    const plain = await enrollWith(await createCode(env, 'acme'));
    expect(plain.config).toMatchObject({ version: 0 });
  });
});
