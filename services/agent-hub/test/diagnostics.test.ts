import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { sha256Hex } from '../src/lib/crypto';
import { canonicalString } from '../src/middleware/signature';
import { MAX_DIAGNOSTICS_BYTES } from '../src/services/diagnostics';
import { admin, b64, enrollAgent, freshNonce, setNow, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

let env: TestEnv;
let agent: TestAgent;

/** A zip-looking body: the local file header magic and some bytes. */
const zip = (size = 64, fill = 7): Uint8Array => {
  const b = new Uint8Array(size).fill(fill);
  b.set([0x50, 0x4b, 0x03, 0x04]);
  return b;
};

/** A signed binary upload, as the agent sends it (Content-Length set by the HTTP client). */
async function upload(body: Uint8Array, opt: { who?: TestAgent; path?: string; length?: string | null } = {}): Promise<Response> {
  const who = opt.who ?? agent;
  const path = opt.path ?? '/v1/agent/diagnostics?command_id=cmd-1';
  const time = String(Math.floor(Date.now() / 1000));
  const nonce = freshNonce();
  const canonical = canonicalString('POST', path, time, nonce, await sha256Hex(body));
  const sig = await crypto.subtle.sign({ name: 'Ed25519' }, who.keys.privateKey, new TextEncoder().encode(canonical));
  const headers: Record<string, string> = {
    'X-Agent-Id': who.id,
    'X-Agent-Time': time,
    'X-Agent-Nonce': nonce,
    'X-Agent-Signature': b64(sig),
    'Content-Type': 'application/zip',
  };
  const length = opt.length === undefined ? String(body.byteLength) : opt.length;
  if (length !== null) headers['Content-Length'] = length;
  return app.request(`https://hub.test${path}`, { method: 'POST', headers, body }, env);
}

interface Stored {
  id: string;
  key: string;
  size: number;
  sha256: string;
}

beforeEach(async () => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
  agent = await enrollAgent(env, 'acme');
});
afterEach(() => vi.useRealTimers());

describe('POST /v1/agent/diagnostics', () => {
  it('stores the zip in R2 under diag/<tenant>/<agent>/ with a row admin can list and download', async () => {
    const body = zip(300);
    const res = await upload(body);
    expect(res.status).toBe(200);
    const stored = (await res.json()) as Stored;
    expect(stored.key).toMatch(new RegExp(`^diag/acme/${agent.id}/${T0 * 1000}-[0-9a-f]{8}\\.zip$`));
    expect(stored).toMatchObject({ size: 300, sha256: await sha256Hex(body) });
    expect(env.ARTIFACTS.store.get(stored.key)).toEqual(body);

    const list = (await (await admin(env, 'GET', `/admin/diagnostics?agent_id=${agent.id}`)).json()) as { diagnostics: Record<string, unknown>[] };
    expect(list.diagnostics).toHaveLength(1);
    expect(list.diagnostics[0]).toMatchObject({
      id: stored.id,
      agent_id: agent.id,
      tenant_id: 'acme',
      site_id: 'main',
      r2_key: stored.key,
      size: 300,
      command_id: 'cmd-1',
      agent_version: '1.0.0',
      created_at: T0,
    });

    const dl = await admin(env, 'GET', `/admin/diagnostics/${stored.id}/download`);
    expect(dl.status).toBe(200);
    expect(dl.headers.get('Content-Type')).toBe('application/zip');
    expect(dl.headers.get('Content-Disposition')).toContain(`diag-acme-${agent.id}-${T0}.zip`);
    expect(new Uint8Array(await dl.arrayBuffer())).toEqual(body);
  });

  it('two uploads in the same millisecond get two keys', async () => {
    const a = (await (await upload(zip(10, 1))).json()) as Stored;
    const b = (await (await upload(zip(10, 2))).json()) as Stored;
    expect(a.key).not.toBe(b.key);
    expect(env.ARTIFACTS.store.size).toBe(2);
  });

  it('refuses an oversized or unsized upload before the signature check', async () => {
    const big = await upload(zip(16), { length: String(MAX_DIAGNOSTICS_BYTES + 1) });
    expect(big.status).toBe(413);
    expect(await big.json()).toMatchObject({ error: 'diagnostics_too_large', max_bytes: MAX_DIAGNOSTICS_BYTES });
    const unsized = await upload(zip(16), { length: null });
    expect(unsized.status).toBe(411);
    expect(env.ARTIFACTS.store.size).toBe(0);
  });

  it('caps the body it actually read, needs a zip and a valid signature', async () => {
    const lying = await upload(zip(MAX_DIAGNOSTICS_BYTES + 1), { length: '10' });
    expect(lying.status).toBe(413);
    const text = await upload(new TextEncoder().encode('{"not":"a zip"}'));
    expect(text.status).toBe(400);
    expect(await text.json()).toMatchObject({ error: 'not_a_zip' });
    const other = await enrollAgent(env, 'other');
    const forged = await upload(zip(), { who: { ...agent, keys: other.keys } });
    expect(forged.status).toBe(401);
    const badCommand = await upload(zip(), { path: '/v1/agent/diagnostics?command_id=a%20b' });
    expect(badCommand.status).toBe(400);
    expect(env.ARTIFACTS.store.size).toBe(0);
  });

  it('never serves a bundle on the agent artifact route, not even to its own agent', async () => {
    const stored = (await (await upload(zip())).json()) as Stored;
    const other = await enrollAgent(env, 'other');
    for (const who of [agent, other]) {
      const time = String(Math.floor(Date.now() / 1000));
      const nonce = freshNonce();
      const path = `/v1/artifacts/${stored.key}`;
      const canonical = canonicalString('GET', path, time, nonce, await sha256Hex(''));
      const sig = await crypto.subtle.sign({ name: 'Ed25519' }, who.keys.privateKey, new TextEncoder().encode(canonical));
      const res = await app.request(
        `https://hub.test${path}`,
        { headers: { 'X-Agent-Id': who.id, 'X-Agent-Time': time, 'X-Agent-Nonce': nonce, 'X-Agent-Signature': b64(sig) } },
        env,
      );
      expect(res.status).toBe(404);
    }
  });

  it('lists by tenant, newest first; an unknown id is 404', async () => {
    await upload(zip());
    setNow(T0 + 60);
    const later = (await (await upload(zip())).json()) as Stored;
    const other = await enrollAgent(env, 'other');
    await upload(zip(), { who: other });
    const list = (await (await admin(env, 'GET', '/admin/diagnostics?tenant_id=acme')).json()) as { diagnostics: { id: string }[] };
    expect(list.diagnostics.map((d) => d.id)[0]).toBe(later.id);
    expect(list.diagnostics).toHaveLength(2);
    expect((await admin(env, 'GET', '/admin/diagnostics/nope/download')).status).toBe(404);
  });
});
