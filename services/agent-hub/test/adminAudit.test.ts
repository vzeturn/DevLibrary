import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { clearAccessKeyCache } from '../src/lib/accessJwt';
import { auditBody, isKillSwitch, isSecretName, MAX_AUDIT_BODY } from '../src/middleware/adminAudit';
import { accessSigner } from './accessSigner';
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
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  clearAccessKeyCache();
});

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

  const putAs = (headers: Record<string, string>): Promise<Response> =>
    Promise.resolve(
      app.request(
        'https://hub.test/admin/tenants/acme',
        { method: 'PUT', headers: { Authorization: `Bearer ${ADMIN}`, ...headers }, body: JSON.stringify({ name: 'ACME' }) },
        env,
      ),
    );

  it('never trusts the Access e-mail header by itself', async () => {
    expect((await putAs({ 'Cf-Access-Authenticated-User-Email': 'ops@example.test' })).status).toBe(200);
    env = { ...env, ACCESS_TEAM_DOMAIN: 'team.cloudflareaccess.com', ACCESS_AUD: 'aud-1' };
    vi.stubGlobal('fetch', async () => new Response(JSON.stringify({ keys: [] })));
    await putAs({ 'Cf-Access-Authenticated-User-Email': 'ops@example.test', 'Cf-Access-Jwt-Assertion': 'forged.jwt.token' });
    const rows = await audit();
    expect(rows.map((r) => r.actor)).toEqual(['admin-token', 'admin-token']);
  });

  it('names the user of a verified Cloudflare Access token', async () => {
    const signer = await accessSigner();
    env = { ...env, ACCESS_TEAM_DOMAIN: 'team.cloudflareaccess.com', ACCESS_AUD: 'aud-1' };
    vi.stubGlobal('fetch', async () => new Response(JSON.stringify({ keys: [signer.jwk] })));
    const jwt = await signer.sign({ aud: ['aud-1'], iss: 'https://team.cloudflareaccess.com', exp: T0 + 300, email: 'ops@example.test' });
    expect((await putAs({ 'Cf-Access-Jwt-Assertion': jwt, 'Cf-Access-Authenticated-User-Email': 'someone@else.test' })).status).toBe(200);
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

  it('scrubs names ending in key but keeps object keys and key ids', () => {
    for (const n of ['key', 'tenant_key', 'hmac_key', 'api-key', 'DL_AGENT_ARRIVAL_TENANT_KEY', 'private_key', 'sql_password', 'token', 'secret']) {
      expect(isSecretName(n), n).toBe(true);
    }
    for (const n of ['r2_key', 'object_key', 'public_key', 'pubkey', 'key_id', 'kid', 'keys', 'monkey_count', 'sort_key']) {
      expect(isSecretName(n), n).toBe(false);
    }
    const body = auditBody(
      JSON.stringify({ tenant_key: 'tk-1', hmac_key: 'hk-1', r2_key: 'diag/a.zip', key_id: 'kid-1', public_key: 'pk', nested: [{ key: 'k-1' }] }),
    );
    expect(JSON.parse(body ?? '')).toEqual({
      tenant_key: '[redacted]',
      hmac_key: '[redacted]',
      r2_key: 'diag/a.zip',
      key_id: 'kid-1',
      public_key: 'pk',
      nested: [{ key: '[redacted]' }],
    });
    const text = auditBody('env DL_AGENT_ARRIVAL_TENANT_KEY=tk-2 key=abc r2_key=diag/x hmac_key: "hk 2"') ?? '';
    expect(text).not.toMatch(/tk-2|abc|hk 2/);
    expect(text).toContain('r2_key=diag/x');
    expect(text).toContain('hmac_key: "[redacted]"');
  });

  it('follows ADO.NET quoting for connection-string passwords', () => {
    expect(auditBody('Server=db;Password="a""b;c";Database=x')).toBe('Server=db;Password="[redacted]";Database=x');
    expect(auditBody("Server=db;Pwd='it''s';Database=x")).toBe("Server=db;Pwd='[redacted]';Database=x");
    expect(auditBody('Server=db;Pwd=a b,c;Database=x')).toBe('Server=db;Pwd=[redacted];Database=x');
    const json = auditBody(JSON.stringify({ connection: 'Server=db;Password="a""b";User Id=sa' })) ?? '';
    expect(json).not.toContain('a\\"\\"b');
    expect(JSON.parse(json)).toEqual({ connection: 'Server=db;Password="[redacted]";User Id=sa' });
  });

  it('refuses the change with 503 when the audit row cannot be written', async () => {
    env.DB.raw.exec('DROP TABLE admin_audit');
    const res = await admin(env, 'PUT', '/admin/tenants/acme', { name: 'ACME' });
    expect(res.status).toBe(503);
    expect(await res.json()).toEqual({ error: 'audit_unavailable' });
    const tenants = (await (await admin(env, 'GET', '/admin/tenants')).json()) as { tenants: unknown[] };
    expect(tenants.tenants).toHaveLength(0);
  });

  it('lets the kill switches through when the audit row cannot be written', async () => {
    expect((await admin(env, 'PUT', '/admin/tenants/acme', { name: 'ACME' })).status).toBe(200);
    env.DB.raw.exec('DROP TABLE admin_audit');
    const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    expect((await admin(env, 'POST', '/admin/tenants/acme/hold', { reason: 'bad build' })).status).toBe(201);
    expect((await admin(env, 'DELETE', '/admin/tenants/acme/hold', {})).status).toBe(200);
    // Reaching the handler (404, not 503) is what matters for these two.
    expect((await admin(env, 'POST', '/admin/releases/dl-agent/9.9.9/pause', {})).status).toBe(404);
    expect((await admin(env, 'POST', '/admin/agents/nope/revoke')).status).toBe(404);
    expect((await admin(env, 'PUT', '/admin/tenants/acme', { name: 'Other' })).status).toBe(503);
    const failOpen = errors.mock.calls.map((c) => JSON.parse(String(c[0])) as Record<string, unknown>).filter((l) => l.fail_open === true);
    expect(failOpen.map((l) => `${String(l.method)} ${String(l.path)}`)).toEqual([
      'POST /admin/tenants/acme/hold',
      'DELETE /admin/tenants/acme/hold',
      'POST /admin/releases/dl-agent/9.9.9/pause',
      'POST /admin/agents/nope/revoke',
    ]);
    expect(failOpen[0]).toMatchObject({ event: 'admin_audit_write_failed', actor: 'admin-token', body: '{"reason":"bad build"}' });
    errors.mockRestore();
  });

  it('knows which routes are kill switches', () => {
    expect(isKillSwitch('POST', '/admin/releases/dl-agent/1.2.3/pause')).toBe(true);
    expect(isKillSwitch('POST', '/admin/releases/dl-agent/1.2.3/resume')).toBe(true);
    expect(isKillSwitch('DELETE', '/admin/tenants/acme/hold')).toBe(true);
    expect(isKillSwitch('POST', '/admin/agents/a-1/revoke')).toBe(true);
    expect(isKillSwitch('PUT', '/admin/tenants/acme/hold')).toBe(false);
    expect(isKillSwitch('POST', '/admin/releases')).toBe(false);
    expect(isKillSwitch('POST', '/admin/agents/a-1/revoke/extra')).toBe(false);
  });

  it('an unauthenticated change is refused and not recorded', async () => {
    const res = await app.request('https://hub.test/admin/tenants/acme', { method: 'PUT', body: '{"name":"x"}' }, env);
    expect(res.status).toBe(401);
    expect(await audit()).toHaveLength(0);
  });
});
