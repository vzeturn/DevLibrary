import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { hubUrls, MAX_HUB_URLS, primaryHost, servedOnFallback, validHubUrl } from '../src/domain/hubUrls';
import { ADMIN, createCode, createTenant, enrollAgent, enrollRequest, heartbeatBody, newKeys, setNow, signed, T0, testEnv, type TestEnv } from './helpers';

const LIST = 'https://hub.test, https://agent-hub.example.workers.dev/';

let env: TestEnv;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv({ HUB_URLS: LIST });
});
afterEach(() => vi.useRealTimers());

describe('hub host list', () => {
  it('keeps https (http only on loopback), no credentials or query, deduplicated and capped', () => {
    expect(validHubUrl(' https://agent.pms.id.vn/ ')).toBe('https://agent.pms.id.vn');
    expect(validHubUrl('http://127.0.0.1:8787')).toBe('http://127.0.0.1:8787');
    for (const bad of ['http://agent.pms.id.vn', 'https://u:p@x.test', 'https://x.test/?a=1', 'https://x.test/#f', 'ftp://x', 'x.test']) {
      expect(validHubUrl(bad), bad).toBeNull();
    }
    expect(hubUrls('https://a.test,https://a.test/, http://evil.test ,')).toEqual(['https://a.test']);
    expect(hubUrls(Array.from({ length: 8 }, (_, i) => `https://h${i}.test`).join(','))).toHaveLength(MAX_HUB_URLS);
    expect(hubUrls(undefined)).toEqual([]);
    expect(primaryHost(LIST)).toBe('hub.test');
    expect(primaryHost('')).toBeNull();
    expect(servedOnFallback('/v1/heartbeat')).toBe(true);
    expect(servedOnFallback('/admin/agents')).toBe(false);
  });

  it('hands the list out on enrol and on every heartbeat', async () => {
    const agent = await enrollAgent(env, 'acme');
    const answer = (await (await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent))).json()) as { hub_urls?: string[] };
    expect(answer.hub_urls).toEqual(['https://hub.test', 'https://agent-hub.example.workers.dev']);
  });

  it('enrol answers carry the list', async () => {
    await createTenant(env, 'acme', false);
    const { publicKey } = await newKeys();
    const res = await enrollRequest(env, { code: await createCode(env, 'acme'), public_key: publicKey, hostname: 'FO', os: 'windows', arch: 'x86_64', version: '1.0.0', modules: [] });
    expect(res.status).toBe(200);
    expect(((await res.json()) as { hub_urls?: string[] }).hub_urls).toEqual(['https://hub.test', 'https://agent-hub.example.workers.dev']);
  });

  it('serves only agent paths on the fallback host', async () => {
    const agent = await enrollAgent(env, 'acme');
    const fallback = 'https://agent-hub.example.workers.dev';
    const adminCall = (base: string) => app.request(`${base}/admin/agents`, { headers: { Authorization: `Bearer ${ADMIN}` } }, env);
    expect((await adminCall('https://hub.test')).status).toBe(200);
    expect((await adminCall(fallback)).status).toBe(404);
    expect((await app.request(`${fallback}/bench/x`, {}, env)).status).toBe(404);
    expect((await app.request(`${fallback}/healthz`, {}, env)).status).toBe(200);
    // A signed heartbeat reaches the fallback (the signature covers the path, not the host).
    const res = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent), { base: fallback });
    expect(res.status).toBe(200);
    const script = await (await app.request(`${fallback}/install.ps1`, {}, env)).text();
    expect(script).toContain(`[string]$Hub = '${fallback}'`);
    expect(script).toContain(`('https://hub.test,${fallback}' -split ',')`);
    expect(script).not.toContain('__HUB');
  });

  it('without HUB_URLS every host serves everything and answers carry no list', async () => {
    const plain = testEnv();
    const agent = await enrollAgent(plain, 'acme');
    const answer = (await (await signed(plain, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent))).json()) as Record<string, unknown>;
    expect(answer).not.toHaveProperty('hub_urls');
    const res = await app.request('https://anything.test/admin/agents', { headers: { Authorization: `Bearer ${ADMIN}` } }, plain);
    expect(res.status).toBe(200);
  });
});
