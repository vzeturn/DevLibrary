import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { admin, enrollAgent, freshNonce, heartbeatBody, newKeys, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

let env: TestEnv;
let agent: TestAgent;

beforeEach(async () => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
  agent = await enrollAgent(env, 'acme');
});
afterEach(() => vi.useRealTimers());

describe('signature middleware', () => {
  it('accepts a correctly signed request', async () => {
    const res = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent));
    expect(res.status).toBe(200);
  });

  it('rejects a signature made with another key', async () => {
    const other = await newKeys();
    const res = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent), { signWith: other.keys });
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: 'bad_signature' });
  });

  it('rejects a body changed after signing', async () => {
    const res = await signed(env, agent, 'POST', '/v1/reports', { reports: [] }, { sendBody: '{"reports": []}' });
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: 'bad_signature' });
  });

  it('answers clock_skew with the server time beyond 300 s', async () => {
    const res = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent), { time: T0 - 301 });
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: 'clock_skew', server_time: T0 });
    const edge = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent), { time: T0 + 300 });
    expect(edge.status).toBe(200);
  });

  it('rejects a replayed nonce within 10 minutes', async () => {
    const nonce = freshNonce();
    expect((await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent), { nonce })).status).toBe(200);
    const again = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent), { nonce });
    expect(again.status).toBe(401);
    expect(await again.json()).toEqual({ error: 'replay' });
  });

  it('rejects revoked and unknown agents', async () => {
    expect((await admin(env, 'POST', `/admin/agents/${agent.id}/revoke`)).status).toBe(200);
    const revoked = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent));
    expect(revoked.status).toBe(401);
    expect(await revoked.json()).toEqual({ error: 'revoked' });
    const unknown = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent), { agentId: 'nope' });
    expect(await unknown.json()).toEqual({ error: 'unknown_agent' });
  });

  it('rejects unsigned requests', async () => {
    const { app } = await import('../src/index');
    const res = await app.request('https://hub.test/v1/heartbeat', { method: 'POST', body: '{}' }, env);
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: 'unsigned' });
  });
});
