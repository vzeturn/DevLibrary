import type { Env } from '../env';
import { sqlNamesOf, type BenchBody, type BenchPayload } from '../domain/bench';
import { benchScript } from '../install/benchScript';
import { importAesKey, randomToken, seal, sha256Hex, unseal } from '../lib/crypto';
import { HttpError, logError } from '../lib/http';
import { insertBench, listBench, revokeBench, takeFetch, type BenchListRow } from '../repo/bench';
import { revokeCode } from '../repo/enrollCodes';
import { getTenant, upsertTenant } from '../repo/tenants';
import { createEnrollCode } from './admin';

/** 32 random bytes, base64url: the capability in the `irm` link. */
const TOKEN = /^[A-Za-z0-9_-]{43}$/;

async function benchKey(env: Env): Promise<CryptoKey> {
  const key = await importAesKey(env.BENCH_KEY);
  if (!key) throw new HttpError(503, 'bench_key_missing');
  return key;
}

/**
 * Creates a bench bundle: the tenant (created as a demo tenant, so its agents follow canary), an
 * enrolment code living as long as the bundle, and the sealed steps. The token is returned once,
 * inside the `irm` line; D1 keeps only its SHA-256 as the bundle id.
 */
export async function createBench(env: Env, body: BenchBody, origin: string, now: number): Promise<{ id: string; irm: string; expires_at: number }> {
  const key = await benchKey(env);
  const tenant = await getTenant(env.DB, body.tenant_id);
  if (!tenant) {
    await upsertTenant(env.DB, { id: body.tenant_id, name: body.tenant_id, demo: true }, now);
  } else if (tenant.demo !== 1) {
    // Turning a live tenant into a demo one would move all its agents to canary.
    throw new HttpError(409, 'tenant_not_demo');
  }

  const ttl = body.expires_hours * 3600;
  const code = await createEnrollCode(
    env,
    { tenant_id: body.tenant_id, site_id: body.site_id, ttl_seconds: ttl, ...(body.config ? { config: body.config } : {}) },
    now,
  );
  const token = randomToken(32);
  const id = await sha256Hex(token);
  const payload: BenchPayload = { code: code.code, steps: body.steps, sql_databases: body.sql_databases, sql_fixups: body.sql_fixups };
  const expires_at = now + ttl;
  try {
    await insertBench(env.DB, {
      id,
      tenant_id: body.tenant_id,
      site_id: body.site_id,
      label: body.label,
      enroll_code_id: code.id,
      steps_enc: await seal(key, JSON.stringify(payload), id),
      created_at: now,
      expires_at,
      max_fetches: body.max_fetches,
    });
  } catch (err) {
    // The code was never shown to anyone, but must not stay usable without its bundle.
    await revokeCode(env.DB, code.id, now);
    throw err;
  }
  return { id, irm: `irm ${origin}/bench/${token} | iex`, expires_at };
}

export type BenchStatus = 'active' | 'expired' | 'revoked' | 'used_up';

export async function benchView(env: Env, now: number): Promise<(BenchListRow & { status: BenchStatus })[]> {
  return (await listBench(env.DB)).map((b) => {
    let status: BenchStatus = 'active';
    if (b.revoked_at !== null) status = 'revoked';
    else if (b.expires_at <= now) status = 'expired';
    else if (b.fetch_count >= b.max_fetches) status = 'used_up';
    return { ...b, status };
  });
}

export async function revokeBenchBundle(env: Env, id: string, now: number): Promise<void> {
  if (!(await revokeBench(env.DB, id, now))) throw new HttpError(404, 'bench_not_found');
}

/** The script for a token, counting the fetch; null when unknown, expired, revoked or used up. */
export async function fetchBenchScript(env: Env, token: string, origin: string, now: number): Promise<string | null> {
  if (!TOKEN.test(token)) return null;
  const key = await benchKey(env);
  const row = await takeFetch(env.DB, await sha256Hex(token), now);
  if (!row) return null;
  let payload: BenchPayload;
  try {
    payload = JSON.parse(await unseal(key, row.steps_enc, row.id)) as BenchPayload;
  } catch (err) {
    logError('bench_unseal_failed', err, { bench_id: row.id });
    throw new HttpError(500, 'bench_unreadable');
  }
  return benchScript({
    hub: origin,
    label: row.label,
    tenant_id: row.tenant_id,
    site_id: row.site_id,
    expires_at: row.expires_at,
    code: payload.code,
    steps: payload.steps,
    sql_databases: payload.sql_databases,
    sql_fixups: payload.sql_fixups ?? [],
    sql_names: sqlNamesOf(payload),
  });
}
