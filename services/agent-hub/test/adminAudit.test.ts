import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { auditBody, MAX_AUDIT_BODY } from '../src/middleware/adminAudit';
import { ADMIN, admin, setNow, T0, testEnv, type TestEnv } from './helpers';

let env: TestEnv;

interface Row {
  id: number;
  at: number;
  actor: string;
  method: string;
  path: string;
  body: string | null;
  status: number | null;
  error: string | null;
}

const audit = async (query = ''): Promise<Row[]> => ((await (await admin(env, 'GET', `/admin/audit${query}`)).json()) as { audit: Row[] }).audit;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
});
afterEach(() => vi.useRealTimers());

describe('admin audit', () => {
  it('records each change with who, what and the outcome; reads are not recorded', async () => {
    expect((await admin(env, 'PUT', '/admin/tenants/acme', { name: 'ACME', demo: false })).status).toBe(200);
    await admin(env, 'GET', '/admin/tenants');
    const bad = await admin(env, 'PUT', '/admin/tenants/acme', { demo: 'nope' });
    expect(bad.status).toBe(400);
    const rows = await audit();
    expect(rows).toHaveLength(2);
    expect(rows[1]).toMatchObject({ at: T0, actor: 'admin-token', method: 'PUT', path: '/admin/tenants/acme', status: 200, error: null });
    expect(JSON.parse(rows[1]?.body ?? '')).toEqual({ name: 'ACME', demo: false });
    expect(rows[0]).toMatchObject({ method: 'PUT', status: 400, error: 'invalid_body' });
  });

  it('names the Cloudflare Access user when there is one', async () => {
    await app.request(
      'https://hub.test/admin/tenants/acme',
      {
        method: 'PUT',
        headers: { Authorization: `Bearer ${ADMIN}`, 'Cf-Access-Authenticated-User-Email': 'ops@example.test' },
        body: JSON.stringify({ name: 'ACME' }),
      },
      env,
    );
    expect((await audit('?actor=ops%40example.test'))[0]).toMatchObject({ actor: 'ops@example.test', status: 200 });
  });

  it('never stores a secret from the body', async () => {
    await admin(env, 'PUT', '/admin/tenants/acme', { name: 'ACME' });
    const res = await admin(env, 'POST', '/admin/enroll-codes', {
      tenant_id: 'acme',
      site_id: 'main',
      config: {
        modules: { sql: { connection: 'Server=db;User Id=sa;Password=Hunter2;', sql_password: 'Hunter2', api_token: 'tok-123' } },
        note: 'Authorization: Bearer abc.def',
      },
    });
    expect(res.status).toBe(201);
    const row = (await audit('?path_prefix=/admin/enroll-codes'))[0];
    expect(row?.body).toBeTruthy();
    expect(row?.body).not.toContain('Hunter2');
    expect(row?.body).not.toContain('tok-123');
    expect(row?.body).not.toContain('abc.def');
    expect(row?.body).toContain('Password=[redacted]');
    expect(row?.body).toContain('"sql_password":"[redacted]"');
  });

  it('cuts a long body and scrubs a body that is not JSON', () => {
    const long = auditBody(JSON.stringify({ notes: 'x'.repeat(MAX_AUDIT_BODY * 2) })) ?? '';
    expect(long.length).toBeLessThanOrEqual(MAX_AUDIT_BODY + 10);
    expect(long.endsWith('…[cut]')).toBe(true);
    expect(auditBody('pwd=abc; other')).toBe('pwd=[redacted]; other');
    expect(auditBody('   ')).toBeNull();
  });

  it('refuses the change with 503 when the audit row cannot be written', async () => {
    env.DB.raw.exec('DROP TABLE admin_audit');
    const res = await admin(env, 'PUT', '/admin/tenants/acme', { name: 'ACME' });
    expect(res.status).toBe(503);
    expect(await res.json()).toEqual({ error: 'audit_unavailable' });
    const tenants = (await (await admin(env, 'GET', '/admin/tenants')).json()) as { tenants: unknown[] };
    expect(tenants.tenants).toHaveLength(0);
  });

  it('an unauthenticated change is refused and not recorded', async () => {
    const res = await app.request('https://hub.test/admin/tenants/acme', { method: 'PUT', body: '{"name":"x"}' }, env);
    expect(res.status).toBe(401);
    expect(await audit()).toHaveLength(0);
  });
});
