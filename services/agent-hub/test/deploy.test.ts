import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { BOOT_HOST_SUFFIXES, bootCommand, CODE_SLOT, PRODUCT_LABELS, productDeployUrl, safeBootUrl } from '../src/install/deployPage';
import { admin, createTenant, enrollRequest, heartbeatBody, newKeys, setNow, signed, T0, testEnv, type TestEnv } from './helpers';

let env: TestEnv;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
});
afterEach(() => vi.useRealTimers());

async function status(code: string): Promise<Record<string, unknown>> {
  const res = await app.request('https://hub.test/deploy/status', { method: 'POST', body: JSON.stringify({ code }) }, env);
  expect(res.status).toBe(200);
  expect(res.headers.get('Cache-Control')).toBe('no-store');
  return (await res.json()) as Record<string, unknown>;
}

async function productCode(product: string): Promise<{ code: string; deploy_url: string; install_command: string }> {
  const key = (await (await admin(env, 'POST', '/admin/product-keys', { product, label: product })).json()) as { key: string };
  const res = await app.request(
    `https://hub.test/products/${product}/enroll-codes`,
    { method: 'POST', headers: { Authorization: `Bearer ${key.key}`, 'Content-Type': 'application/json' }, body: JSON.stringify({ external_id: 'h1', name: 'Hotel 1' }) },
    env,
  );
  expect(res.status).toBe(201);
  return (await res.json()) as { code: string; deploy_url: string; install_command: string };
}

describe('/deploy guided install', () => {
  it('serves one page with the install line template, a nonce CSP and no code in it', async () => {
    const res = await app.request('https://hub.test/deploy', {}, env);
    expect(res.status).toBe(200);
    const csp = res.headers.get('Content-Security-Policy') ?? '';
    const nonce = /script-src 'nonce-([0-9a-f]{32})'/.exec(csp)?.[1];
    expect(nonce).toBeDefined();
    expect(csp).toContain("frame-ancestors 'none'");
    expect(res.headers.get('Referrer-Policy')).toBe('no-referrer');
    const html = await res.text();
    expect(html).toContain(`<script nonce="${nonce}">`);
    expect(html).toContain(`DownloadString('https://hub.test/install.ps1'))) -Code '${CODE_SLOT}'`);
    expect(html).toContain('Run as administrator');
    // Every product that mints codes has a display name.
    for (const p of ['einvoice', 'arrival', 'membership', 'pms-site', 'pda-sync', 'hotellink', 'backup']) expect(PRODUCT_LABELS[p]).toBeTruthy();
  });

  it('new codes come with a deploy_url carrying the code in the fragment only', async () => {
    const minted = await productCode('arrival');
    expect(minted.deploy_url).toBe(`https://hub.test/deploy#code=${minted.code}`);
    await createTenant(env, 'plain', false);
    const res = await admin(env, 'POST', '/admin/enroll-codes', { tenant_id: 'plain', site_id: 'main' });
    const created = (await res.json()) as { code: string; deploy_url: string };
    expect(created.deploy_url).toBe(`https://hub.test/deploy#code=${created.code}`);
  });

  it('tracks a code: waiting, then the machine that used it with modules and self-test', async () => {
    const minted = await productCode('arrival');
    expect(await status(minted.code.toLowerCase())).toMatchObject({ state: 'waiting', product: 'arrival', site: 'main', expires_at: T0 + 86400, agent: null });

    const { keys, publicKey } = await newKeys();
    const res = await enrollRequest(env, { code: minted.code, public_key: publicKey, hostname: 'MNB-FO', os: 'windows', arch: 'x86_64', version: '1.3.16', modules: ['arrival'] });
    const { agent_id } = (await res.json()) as { agent_id: string };
    const agent = { id: agent_id, keys, publicKey };
    expect(await status(minted.code)).toMatchObject({ state: 'connected', agent: { hostname: 'MNB-FO', online: false, modules: [], selftest: null } });

    const hb = heartbeatBody(agent, '1.3.16', {
      modules: [
        { name: 'sys', state: 'running' },
        { name: 'arrival', state: 'running' },
      ],
    });
    expect((await signed(env, agent, 'POST', '/v1/heartbeat', hb)).status).toBe(200);
    const run = {
      run_id: 'r1',
      trigger: 'install',
      started_at: T0,
      finished_at: T0 + 5,
      agent_version: '1.3.16',
      checks: [
        { id: 'hub', module: 'sys', title: 'Hub', status: 'pass', duration_ms: 10 },
        { id: 'sql', module: 'arrival', title: 'SQL', status: 'warn', duration_ms: 20 },
      ],
    };
    const st = await signed(env, agent, 'POST', '/v1/agent/selftest', run);
    expect(st.status, await st.clone().text()).toBe(200);
    expect(await status(minted.code)).toMatchObject({
      state: 'connected',
      agent: { hostname: 'FO-SERVER', version: '1.3.16', online: true, modules: [{ name: 'arrival', state: 'running' }], selftest: { pass: 1, warn: 1, fail: 0, total: 2 } },
    });
  });

  it('says unknown, expired or revoked, and nothing else', async () => {
    expect(await status('NOPE-NOPE')).toEqual({ state: 'unknown', product: null, site: null, expires_at: null, agent: null });
    const minted = await productCode('einvoice');
    setNow(T0 + 86401);
    expect(await status(minted.code)).toMatchObject({ state: 'expired', agent: null });
    setNow(T0);
    const codes = (await (await admin(env, 'GET', '/admin/enroll-codes')).json()) as { codes: { id: string }[] };
    expect((await admin(env, 'DELETE', `/admin/enroll-codes/${codes.codes[0]?.id}`)).status).toBe(200);
    expect(await status(minted.code)).toMatchObject({ state: 'revoked' });
    const bad = await app.request('https://hub.test/deploy/status', { method: 'POST', body: '{}' }, env);
    expect(bad.status).toBe(400);
  });

  it('is served on the primary host only', async () => {
    env = testEnv({ HUB_URLS: 'https://hub.test,https://fallback.test' });
    expect((await app.request('https://fallback.test/deploy', {}, env)).status).toBe(404);
    expect((await app.request('https://hub.test/deploy', {}, env)).status).toBe(200);
  });

  it('runs a product console\'s own install script only from Azolo product hosts', () => {
    const ok = 'https://einvoice.mmsolutions.vn/agent/bootstrap/tok_1-a/dl/ABCD-EFGH';
    expect(safeBootUrl(ok, BOOT_HOST_SUFFIXES)).toBe(ok);
    expect(safeBootUrl('https://arrival.pms.id.vn/agent/install.ps1', BOOT_HOST_SUFFIXES)).not.toBe('');
    for (const bad of [
      'http://einvoice.mmsolutions.vn/x', // not https
      'https://evil.example/x',
      'https://pms.id.vn.evil.example/x',
      'https://evilpms.id.vn/x', // suffix without the dot
      "https://a.pms.id.vn/x');Remove-Item C:\\ -Recurse;('", // quote breaks out of the PowerShell string
      'https://a.pms.id.vn/x?token=1', // no query
      'https://a.pms.id.vn:8443/x',
      'https://a..pms.id.vn/x',
      'https://a.pms.id.vn',
      '',
      null,
    ])
      expect(safeBootUrl(bad, BOOT_HOST_SUFFIXES), String(bad)).toBe('');
    expect(bootCommand(ok)).toContain(`DownloadString('${ok}')`);
    // The page embeds the function verbatim: it must not depend on anything outside itself.
    const embedded = new Function(`return (${safeBootUrl.toString()})`)() as typeof safeBootUrl;
    expect(embedded(ok, BOOT_HOST_SUFFIXES)).toBe(ok);
    expect(embedded('https://evil.example/x', BOOT_HOST_SUFFIXES)).toBe('');
  });

  it('builds a product link with code, script and the remote-SQL variant in the fragment only', async () => {
    const url = productDeployUrl('https://hub.test', { code: 'ABCD-EFGH', boot: 'https://e.mmsolutions.vn/b/t/dl/ABCD-EFGH', alt: 'https://e.mmsolutions.vn/b/t/dl/ABCD-EFGH/remote' });
    const u = new URL(url);
    expect(u.pathname).toBe('/deploy');
    expect(u.search).toBe('');
    const h = new URLSearchParams(u.hash.slice(1));
    expect([h.get('code'), h.get('boot'), h.get('alt')]).toEqual(['ABCD-EFGH', 'https://e.mmsolutions.vn/b/t/dl/ABCD-EFGH', 'https://e.mmsolutions.vn/b/t/dl/ABCD-EFGH/remote']);
    const html = await (await app.request('https://hub.test/deploy', {}, env)).text();
    expect(html).toContain('var safeBootUrl=function safeBootUrl(');
    expect(html).toContain(`var SUFFIXES=${JSON.stringify(BOOT_HOST_SUFFIXES)}`);
    expect(html).toContain('SQL Server nằm trên máy khác');
  });
});
