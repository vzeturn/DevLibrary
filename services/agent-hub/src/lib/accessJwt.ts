/**
 * Cloudflare Access application token (`Cf-Access-Jwt-Assertion`) verification, used only to
 * name the admin in the audit trail. The `Cf-Access-Authenticated-User-Email` header alone is
 * never trusted: a request that reaches the Worker without passing Access (workers.dev, a
 * misrouted host) could set it to anything.
 *
 * Configuration (wrangler vars, both or nothing): `ACCESS_TEAM_DOMAIN` (`<team>.cloudflareaccess.com`)
 * and `ACCESS_AUD` (the Access application's audience tag). Without them no token is trusted.
 */

/** What a verified token says about the caller. */
export interface AccessIdentity {
  /** The user's e-mail, or `service-token:<client id>` for an Access service token. */
  actor: string;
}

export interface AccessConfig {
  teamDomain: string;
  aud: string;
}

interface Jwk extends JsonWebKey {
  kid?: string;
}

/** Fetches the team's signing keys (`/cdn-cgi/access/certs`). */
export type JwksFetcher = (url: string) => Promise<{ keys: Jwk[] }>;

const JWKS_TTL_MS = 60 * 60 * 1000;
/** Clock skew allowed on `exp` / `nbf`. */
const LEEWAY_SECONDS = 60;

const cache = new Map<string, { at: number; keys: Jwk[] }>();

/** Clears the signing-key cache (tests). */
export function clearAccessKeyCache(): void {
  cache.clear();
}

const defaultFetcher: JwksFetcher = async (url) => {
  const res = await fetch(url, { headers: { accept: 'application/json' } });
  if (!res.ok) throw new Error(`access certs answered ${res.status}`);
  return (await res.json()) as { keys: Jwk[] };
};

/** The Access config from env, or null when it is not (fully) set. */
export function accessConfig(env: { ACCESS_TEAM_DOMAIN?: string; ACCESS_AUD?: string }): AccessConfig | null {
  const teamDomain = env.ACCESS_TEAM_DOMAIN?.trim().replace(/^https:\/\//, '').replace(/\/+$/, '');
  const aud = env.ACCESS_AUD?.trim();
  return teamDomain && aud ? { teamDomain, aud } : null;
}

function b64urlBytes(s: string): Uint8Array {
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (s.length % 4)) % 4);
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function b64urlJson(s: string): Record<string, unknown> {
  const value = JSON.parse(new TextDecoder().decode(b64urlBytes(s))) as unknown;
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('not an object');
  return value as Record<string, unknown>;
}

async function signingKeys(cfg: AccessConfig, fetcher: JwksFetcher, refresh: boolean): Promise<Jwk[]> {
  const hit = cache.get(cfg.teamDomain);
  if (hit && !refresh && Date.now() - hit.at < JWKS_TTL_MS) return hit.keys;
  const { keys } = await fetcher(`https://${cfg.teamDomain}/cdn-cgi/access/certs`);
  if (!Array.isArray(keys)) throw new Error('access certs without keys');
  cache.set(cfg.teamDomain, { at: Date.now(), keys });
  return keys;
}

/**
 * The caller named by a valid Access token: RS256 signature by one of the team's keys, `aud`
 * containing the application's tag, `iss` the team domain, inside `nbf`..`exp`. Null for a
 * missing, malformed, expired or foreign token. Errors fetching the keys propagate.
 */
export async function verifyAccessJwt(
  token: string | undefined,
  cfg: AccessConfig,
  nowSeconds: number,
  fetcher: JwksFetcher = defaultFetcher,
): Promise<AccessIdentity | null> {
  if (!token) return null;
  const parts = token.split('.');
  if (parts.length !== 3) return null;
  const [h, p, sig] = parts as [string, string, string];
  let header: Record<string, unknown>;
  let payload: Record<string, unknown>;
  let signature: Uint8Array;
  try {
    header = b64urlJson(h);
    payload = b64urlJson(p);
    signature = b64urlBytes(sig);
  } catch {
    return null; // not a JWT: untrusted, not an error
  }
  if (header.alg !== 'RS256' || typeof header.kid !== 'string') return null;
  let jwk = (await signingKeys(cfg, fetcher, false)).find((k) => k.kid === header.kid);
  // A key rotated since the cache was filled: look once more.
  jwk ??= (await signingKeys(cfg, fetcher, true)).find((k) => k.kid === header.kid);
  if (!jwk) return null;
  const key = await crypto.subtle.importKey(
    'jwk',
    { kty: jwk.kty, n: jwk.n, e: jwk.e, alg: 'RS256', ext: true },
    { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' },
    false,
    ['verify'],
  );
  const ok = await crypto.subtle.verify('RSASSA-PKCS1-v1_5', key, signature, new TextEncoder().encode(`${h}.${p}`));
  if (!ok) return null;
  const aud = Array.isArray(payload.aud) ? payload.aud : [payload.aud];
  if (!aud.includes(cfg.aud)) return null;
  if (payload.iss !== `https://${cfg.teamDomain}`) return null;
  if (typeof payload.exp !== 'number' || payload.exp + LEEWAY_SECONDS < nowSeconds) return null;
  if (typeof payload.nbf === 'number' && payload.nbf - LEEWAY_SECONDS > nowSeconds) return null;
  if (typeof payload.email === 'string' && payload.email) return { actor: payload.email };
  if (typeof payload.common_name === 'string' && payload.common_name) {
    return { actor: `service-token:${payload.common_name}` };
  }
  return null;
}
