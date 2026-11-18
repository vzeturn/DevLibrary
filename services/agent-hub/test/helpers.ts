import { vi } from 'vitest';
import { app } from '../src/index';
import type { Env } from '../src/env';
import { canonicalString } from '../src/middleware/signature';
import { sha256Hex } from '../src/lib/crypto';
import { fakeD1, fakeR2 } from './d1';

export const ADMIN = 'test-admin-token';
export const T0 = 1_763_460_000;

export type TestEnv = Env & { DB: ReturnType<typeof fakeD1> & D1Database; ARTIFACTS: ReturnType<typeof fakeR2> & R2Bucket };

export function testEnv(overrides: Partial<Env> = {}): TestEnv {
  return { DB: fakeD1(), ARTIFACTS: fakeR2(), ADMIN_TOKEN: ADMIN, HEARTBEAT_SECONDS: '60', ...overrides } as unknown as TestEnv;
}

export function b64(bytes: ArrayBuffer | Uint8Array): string {
  return Buffer.from(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)).toString('base64');
}

export interface TestAgent {
  id: string;
  keys: CryptoKeyPair;
  publicKey: string;
}

export async function newKeys(): Promise<{ keys: CryptoKeyPair; publicKey: string }> {
  const keys = (await crypto.subtle.generateKey({ name: 'Ed25519' }, true, ['sign', 'verify'])) as CryptoKeyPair;
  return { keys, publicKey: b64((await crypto.subtle.exportKey('raw', keys.publicKey)) as ArrayBuffer) };
}

const URL_BASE = 'https://hub.test';

export async function admin(env: Env, method: string, path: string, body?: unknown): Promise<Response> {
  const init: RequestInit = { method, headers: { Authorization: `Bearer ${ADMIN}`, 'Content-Type': 'application/json' } };
  if (body !== undefined) init.body = typeof body === 'string' ? body : JSON.stringify(body);
  return app.request(`${URL_BASE}${path}`, init, env);
}

export async function enrollRequest(env: Env, body: Record<string, unknown>): Promise<Response> {
  return app.request(`${URL_BASE}/v1/enroll`, { method: 'POST', body: JSON.stringify(body) }, env);
}

export interface SignOptions {
  time?: number;
  nonce?: string;
  signWith?: CryptoKeyPair;
  agentId?: string;
  /** Send this body instead of the signed one (tampering). */
  sendBody?: string;
}

let nonceCounter = 0;
export function freshNonce(): string {
  nonceCounter += 1;
  return nonceCounter.toString(16).padStart(32, '0');
}

export async function signed(env: Env, agent: TestAgent, method: string, path: string, body?: unknown, opt: SignOptions = {}): Promise<Response> {
  const text = body === undefined ? '' : JSON.stringify(body);
  const time = String(opt.time ?? Math.floor(Date.now() / 1000));
  const nonce = opt.nonce ?? freshNonce();
  const canonical = canonicalString(method, path, time, nonce, await sha256Hex(text));
  const key = (opt.signWith ?? agent.keys).privateKey;
  const sig = await crypto.subtle.sign({ name: 'Ed25519' }, key, new TextEncoder().encode(canonical));
  const headers = {
    'X-Agent-Id': opt.agentId ?? agent.id,
    'X-Agent-Time': time,
    'X-Agent-Nonce': nonce,
    'X-Agent-Signature': b64(sig),
  };
  const init: RequestInit = { method, headers };
  if (body !== undefined) init.body = opt.sendBody ?? text;
  return app.request(`${URL_BASE}${path}`, init, env);
}

export async function createTenant(env: Env, id: string, demo: boolean): Promise<void> {
  const res = await admin(env, 'PUT', `/admin/tenants/${id}`, { name: id.toUpperCase(), demo });
  if (res.status !== 200) throw new Error(`tenant ${res.status}`);
}

export async function createCode(env: Env, tenant: string, site = 'main', ttl = 3600): Promise<string> {
  const res = await admin(env, 'POST', '/admin/enroll-codes', { tenant_id: tenant, site_id: site, ttl_seconds: ttl });
  if (res.status !== 201) throw new Error(`code ${res.status} ${await res.text()}`);
  return ((await res.json()) as { code: string }).code;
}

export async function enrollAgent(env: Env, tenant: string, opts: { demo?: boolean; version?: string } = {}): Promise<TestAgent> {
  await createTenant(env, tenant, opts.demo ?? false);
  const code = await createCode(env, tenant);
  const { keys, publicKey } = await newKeys();
  const res = await enrollRequest(env, {
    code,
    public_key: publicKey,
    hostname: `${tenant}-srv`,
    os: 'windows',
    arch: 'x86_64',
    version: opts.version ?? '1.0.0',
    modules: ['pda-sync'],
  });
  if (res.status !== 200) throw new Error(`enroll ${res.status} ${await res.text()}`);
  const { agent_id } = (await res.json()) as { agent_id: string };
  return { id: agent_id, keys, publicKey };
}

export function heartbeatBody(agent: TestAgent, version = '1.0.0', extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    agent_id: agent.id,
    version,
    channel: 'stable',
    target: 'windows-x86_64',
    hostname: 'FO-SERVER',
    started_at: T0,
    uptime_seconds: 60,
    config_version: 0,
    update: { state: 'idle', version },
    modules: [{ name: 'pda-sync', state: 'running', restarts: 0, metrics: { rows_out: 1 } }],
    ...extra,
  };
}

/** A manifest with deliberately odd spacing, to prove the hub stores and serves text verbatim. */
export function manifestText(version: string, channel: 'canary' | 'stable', publishedAt: number, product = 'dl-agent'): string {
  return `{"product":"${product}", "channel":"${channel}","version":"${version}","published_at":${publishedAt},
 "rollout_percent":100,"min_version":"1.0.0","allow_downgrade":false,"notes":"n",
 "artifacts":[{"target":"windows-x86_64","url":"https://dl.test/a.exe","sha256":"${'ab'.repeat(32)}","size":123}],
 "key_id":"rel-2026","signature":"c2ln"}`;
}

export function setNow(seconds: number): void {
  vi.setSystemTime(new Date(seconds * 1000));
}
