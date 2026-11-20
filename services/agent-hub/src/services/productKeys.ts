import type { Env } from '../env';
import { sha256Hex, uuidv7 } from '../lib/crypto';
import { HttpError } from '../lib/http';
import { getTenant, upsertTenant } from '../repo/tenants';
import { findLiveKey, insertProductKey, touchProductKey, type ProductKeyRow } from '../repo/productKeys';
import { createEnrollCode } from './admin';

/** Prefix that makes a leaked key easy to spot in logs and secret scanners. */
export const KEY_PREFIX = 'dlpk_';

function randomKey(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let bin = '';
  bytes.forEach((b) => (bin += String.fromCharCode(b)));
  return KEY_PREFIX + btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

/** Creates a key for `product`; the plain key is returned once and never stored. */
export async function createProductKey(env: Env, product: string, label: string, now: number): Promise<{ id: string; product: string; key: string }> {
  const key = randomKey();
  const id = uuidv7(now * 1000);
  await insertProductKey(env.DB, { id, product, key_hash: await sha256Hex(key), label, created_at: now });
  return { id, product, key };
}

/** The live key matching the bearer token, or null. */
export async function authenticateProductKey(env: Env, token: string, now: number): Promise<ProductKeyRow | null> {
  if (!token.startsWith(KEY_PREFIX)) return null;
  const row = await findLiveKey(env.DB, await sha256Hex(token));
  if (row) await touchProductKey(env.DB, row.id, now);
  return row;
}

/** Tenants a product key may touch live under `<product>-`, so one product never reaches another's. */
export function productTenantId(product: string, externalId: string): string {
  return `${product}-${externalId}`;
}

export interface ProductCodeRequest {
  external_id: string;
  name: string;
  site_id: string;
  ttl_seconds: number;
  /** Applied only when the tenant is created; an existing tenant keeps its demo flag. */
  demo: boolean;
}

/**
 * Mints a dl-agent enroll code for the product's tenant `external_id` (created on first use). The
 * agent enrolled with it starts with the product's module enabled; an agent already enrolled (for
 * this or another product) attaches it instead (`POST /v1/attach`, RT-0298) and keeps its
 * identity. A code is minted the same way whether or not `product_links` already names an agent
 * for this tenant: attaching again just refreshes the link.
 */
export async function mintProductCode(env: Env, product: string, req: ProductCodeRequest, now: number) {
  const tenantId = productTenantId(product, req.external_id);
  if (tenantId.length > 64) throw new HttpError(400, 'external_id_too_long');
  if (!(await getTenant(env.DB, tenantId))) await upsertTenant(env.DB, { id: tenantId, name: req.name, demo: req.demo }, now);
  return createEnrollCode(
    env,
    {
      tenant_id: tenantId,
      site_id: req.site_id,
      ttl_seconds: req.ttl_seconds,
      config: { modules: { [product]: { enabled: true } }, log_level: 'info' },
      minted_by: product,
    },
    now,
  );
}
