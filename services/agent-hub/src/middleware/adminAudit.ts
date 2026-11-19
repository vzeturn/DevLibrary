import type { Context, MiddlewareHandler } from 'hono';
import { nowSeconds, type AppEnv } from '../env';
import { accessConfig, verifyAccessJwt } from '../lib/accessJwt';
import { HttpError, logError } from '../lib/http';
import { finishAudit, insertAudit } from '../repo/audit';

/** Longest body kept in the audit trail. */
export const MAX_AUDIT_BODY = 4000;

export const REDACTED = '[redacted]';

/**
 * JSON field names whose value is never stored, matched as the end of a longer name too
 * (`sql_password`, `x-api-key`, `tenant_key`, `hmac_key`, `key`).
 */
const SECRET_NAME = /(password|pwd|secret|token|key|authorization|_sealed|credential)$/i;
/** Names ending in `key` that hold no secret: an R2 object key, a public key, a sort key. */
const HARMLESS_NAME = /((r2|object|public|sort|cache)[_-]?key|pubkey)$/i;

/** Whether a field named `name` holds a secret (`key_id` and `kid` never match). */
export function isSecretName(name: string): boolean {
  return SECRET_NAME.test(name) && !HARMLESS_NAME.test(name);
}

/**
 * `Password=…` / `Pwd=…` inside a connection string, ADO.NET rules: `"…"` or `'…'` with the
 * quote doubled inside, else up to the next `;` or line end (spaces and commas included).
 */
const SECRET_IN_TEXT = /\b((?:password|pwd)\s*=\s*)("(?:[^"]|"")*"|'(?:[^']|'')*'|[^;\r\n]*)/gi;
/** `name=value` / `name: value` in free text for any other secret name (`tenant_key=…`). */
const SECRET_PAIR_IN_TEXT = /\b([A-Za-z0-9_-]*(?:secret|token|key|_sealed|credential))(\s*[=:]\s*)("[^"]*"|'[^']*'|[^\s;,&"'}\]]+)/gi;
const BEARER = /\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]+/gi;

function scrubText(text: string): string {
  return text
    .replace(SECRET_IN_TEXT, (_m, name: string, value: string) => `${name}${quoteLike(value)}`)
    .replace(SECRET_PAIR_IN_TEXT, (m, name: string, sep: string, value: string) =>
      isSecretName(name) ? `${name}${sep}${quoteLike(value)}` : m,
    )
    .replace(BEARER, `$1 ${REDACTED}`);
}

/** {@link REDACTED}, in the quotes `value` had. */
function quoteLike(value: string): string {
  const q = value[0];
  return (q === '"' || q === "'") && value.length > 1 ? `${q}${REDACTED}${q}` : REDACTED;
}

function scrubValue(value: unknown): unknown {
  if (typeof value === 'string') return scrubText(value);
  if (Array.isArray(value)) return value.map(scrubValue);
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([k, v]) => [k, isSecretName(k) && v !== null ? REDACTED : scrubValue(v)]),
    );
  }
  return value;
}

/** The body as stored: secrets replaced by {@link REDACTED}, cut at {@link MAX_AUDIT_BODY} characters; null when empty. */
export function auditBody(text: string): string | null {
  if (!text.trim()) return null;
  let clean: string;
  try {
    clean = JSON.stringify(scrubValue(JSON.parse(text) as unknown));
  } catch {
    clean = scrubText(text);
  }
  return clean.length > MAX_AUDIT_BODY ? `${clean.slice(0, MAX_AUDIT_BODY)}…[cut]` : clean;
}

/**
 * Kill-switch routes: stopping a bad release or a tenant must work even when the audit table
 * cannot be written (they fail open; every other change fails closed).
 */
const KILL_SWITCH: ReadonlyArray<readonly [string, RegExp]> = [
  ['POST', /^\/admin\/releases\/[^/]+\/[^/]+\/(pause|resume)$/],
  ['POST', /^\/admin\/tenants\/[^/]+\/hold$/],
  ['DELETE', /^\/admin\/tenants\/[^/]+\/hold$/],
  ['POST', /^\/admin\/agents\/[^/]+\/revoke$/],
];

export function isKillSwitch(method: string, path: string): boolean {
  return KILL_SWITCH.some(([m, re]) => m === method && re.test(path));
}

/**
 * Who made the change: the e-mail (or `service-token:<id>`) from a verified Cloudflare Access
 * token when Access is configured, else `admin-token`. The `Cf-Access-Authenticated-User-Email`
 * header alone is never trusted.
 */
async function auditActor(c: Context<AppEnv>): Promise<string> {
  const cfg = accessConfig(c.env);
  if (!cfg) return 'admin-token';
  try {
    const who = await verifyAccessJwt(c.req.header('Cf-Access-Jwt-Assertion'), cfg, nowSeconds());
    return who?.actor ?? 'admin-token';
  } catch (err) {
    logError('access_jwt_verify_failed', err);
    return 'admin-token';
  }
}

/**
 * Records every admin request that changes something (any method but GET / HEAD) in
 * `admin_audit`. The row is written before the handler runs: if it cannot be written the change
 * is refused with 503 `audit_unavailable`, so no change goes unrecorded, except on the
 * kill-switch routes ({@link isKillSwitch}), which log the would-be row and go ahead. Runs
 * after admin auth.
 */
export const auditAdmin: MiddlewareHandler<AppEnv> = async (c, next) => {
  if (c.req.method === 'GET' || c.req.method === 'HEAD') return next();
  const url = new URL(c.req.url);
  const row = {
    at: nowSeconds(),
    actor: await auditActor(c),
    method: c.req.method,
    path: url.pathname + url.search,
    body: auditBody(await c.req.text()),
  };
  let id: number;
  try {
    id = await insertAudit(c.env.DB, row);
  } catch (err) {
    if (isKillSwitch(c.req.method, url.pathname)) {
      // The structured log line is the trail for this change.
      logError('admin_audit_write_failed', err, { ...row, fail_open: true });
      return next();
    }
    logError('admin_audit_write_failed', err, { method: c.req.method, path: url.pathname });
    throw new HttpError(503, 'audit_unavailable');
  }
  let status = 500;
  let error: string | null = null;
  try {
    await next();
    status = c.res.status;
    if (c.error) error = c.error.message;
  } catch (err) {
    status = err instanceof HttpError ? err.status : 500;
    error = err instanceof Error ? err.message : String(err);
    throw err;
  } finally {
    await finishAudit(c.env.DB, id, status, error).catch((e: unknown) => logError('admin_audit_finish_failed', e, { id, status }));
  }
};
