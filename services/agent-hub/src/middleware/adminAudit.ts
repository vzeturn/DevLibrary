import type { MiddlewareHandler } from 'hono';
import { nowSeconds, type AppEnv } from '../env';
import { HttpError, logError } from '../lib/http';
import { finishAudit, insertAudit } from '../repo/audit';

/** Longest body kept in the audit trail. */
export const MAX_AUDIT_BODY = 4000;

export const REDACTED = '[redacted]';

/** JSON field names whose value is never stored (matched as the end of a longer name too). */
const SECRET_KEY = /(password|pwd|secret|token|api[_-]?key|private[_-]?key|authorization|_sealed|credential)$/i;
/** `Password=…` / `Pwd=…` inside a connection string. */
const SECRET_IN_TEXT = /\b((?:password|pwd)\s*=\s*)("[^"]*"|'[^']*'|[^;"'\s]*)/gi;
const BEARER = /\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]+/gi;

function scrubText(text: string): string {
  return text.replace(SECRET_IN_TEXT, `$1${REDACTED}`).replace(BEARER, `$1 ${REDACTED}`);
}

function scrubValue(value: unknown): unknown {
  if (typeof value === 'string') return scrubText(value);
  if (Array.isArray(value)) return value.map(scrubValue);
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([k, v]) => [k, SECRET_KEY.test(k) && v !== null ? REDACTED : scrubValue(v)]),
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
 * Records every admin request that changes something (any method but GET / HEAD) in
 * `admin_audit`. The row is written before the handler runs: if it cannot be written the change
 * is refused with 503 `audit_unavailable`, so no change goes unrecorded. Runs after admin auth.
 */
export const auditAdmin: MiddlewareHandler<AppEnv> = async (c, next) => {
  if (c.req.method === 'GET' || c.req.method === 'HEAD') return next();
  const url = new URL(c.req.url);
  const actor = c.req.header('Cf-Access-Authenticated-User-Email') ?? 'admin-token';
  let id: number;
  try {
    id = await insertAudit(c.env.DB, {
      at: nowSeconds(),
      actor,
      method: c.req.method,
      path: url.pathname + url.search,
      body: auditBody(await c.req.text()),
    });
  } catch (err) {
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
