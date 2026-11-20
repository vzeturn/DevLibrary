import type { AgentRow, Env } from '../env';
import type { AttachBody } from '../domain/schemas';
import { sha256Hex } from '../lib/crypto';
import { HttpError, logError } from '../lib/http';
import { finishAudit, insertAudit } from '../repo/audit';
import { getConfig } from '../repo/configs';
import { findCodeByHash, findCodeById, type EnrollCodeRow } from '../repo/enrollCodes';
import { writeAttach, type ProductLinkRow } from '../repo/productLinks';
import { DEFAULT_CONFIG, type ConfigView } from './config';
import { normaliseCode } from './enroll';

/** What `POST /v1/attach` answers (PROTOCOL.md §2). */
export interface AttachAnswer {
  /** The agent's config after the merge (unchanged version when nothing changed). */
  config: ConfigView;
  /** The module sections the code carried (`modules.<name>`), e.g. `["pda-sync"]`. */
  attached: string[];
}

/** Times the merge is read and written again when the code or the config moved meanwhile. */
const ATTEMPTS = 3;

type Json = Record<string, unknown>;

function isObject(v: unknown): v is Json {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/** `patch` over `base`: objects merge key by key, anything else in `patch` replaces. */
export function deepMerge(base: unknown, patch: Json): Json {
  const out: Json = isObject(base) ? { ...base } : {};
  for (const [k, v] of Object.entries(patch)) {
    if (k === '__proto__') continue;
    out[k] = isObject(v) ? deepMerge(out[k], v) : v;
  }
  return out;
}

/**
 * The config body with the code's module sections merged in: `modules.<x>` keys the code sets
 * win, other modules and every other top-level key of `body` stay as they are.
 */
export function mergeModules(body: unknown, modules: Json): Json {
  const out: Json = isObject(body) ? { ...body } : {};
  out.modules = deepMerge(out.modules, modules);
  return out;
}

/** The `modules` object of the code's config, `{}` when it carries none. */
function codeModules(code: EnrollCodeRow): Json {
  if (code.config === null) return {};
  const parsed = JSON.parse(code.config) as unknown;
  return isObject(parsed) && isObject(parsed.modules) ? parsed.modules : {};
}

/** The product link a product-minted code (tenant `<product>-<external_id>`) asks for. */
function linkFor(code: EnrollCodeRow, agent: AgentRow): Omit<ProductLinkRow, 'agent_id' | 'created_at'> | null {
  const product = code.minted_by;
  if (product === null || !code.tenant_id.startsWith(`${product}-`)) return null;
  const externalId = code.tenant_id.slice(product.length + 1);
  if (!externalId) return null;
  return { product, external_id: externalId, tenant_id: agent.tenant_id, site_id: agent.site_id };
}

/**
 * `claim`: an unused live code; `held`: the agent already used this code (enrolment or an
 * earlier attach), so attaching again re-merges instead of failing. Anything else is refused
 * like an enrolment with that code.
 */
function codeState(code: EnrollCodeRow | null, agentId: string, now: number): { code: EnrollCodeRow; claim: boolean } {
  if (code && code.used_by_agent === agentId) return { code, claim: false };
  if (!code || code.used_at !== null || code.revoked_at !== null || code.expires_at <= now) {
    throw new HttpError(403, 'enroll_code_invalid');
  }
  return { code, claim: true };
}

/**
 * Attaches the product (module sections) of a one-time code to an enrolled agent: the code's
 * `modules` are merged into the agent's config (version bumped like an admin config change), a
 * product-minted code records the product link, and the code is marked used, all in one
 * transaction. The agent keeps its identity, tenant and site.
 */
export async function attach(env: Env, agent: AgentRow, body: AttachBody, now: number): Promise<AttachAnswer> {
  let state = codeState(await findCodeByHash(env.DB, await sha256Hex(normaliseCode(body.code))), agent.id, now);
  const modules = codeModules(state.code);
  const attached = Object.keys(modules);
  if (state.claim && attached.length === 0) throw new HttpError(400, 'code_has_no_modules');

  const auditId = await openAudit(env.DB, agent, state.code, attached, now);
  try {
    for (let attempt = 1; ; attempt++) {
      const current = await getConfig(env.DB, agent.id);
      const version = current?.version ?? 0;
      const base = current ? (JSON.parse(current.body) as unknown) : DEFAULT_CONFIG.body;
      const merged = mergeModules(base, modules);
      const changed = JSON.stringify(merged) !== JSON.stringify(base);
      const written = await writeAttach(env.DB, {
        codeId: state.code.id,
        agentId: agent.id,
        claim: state.claim,
        expectedVersion: version,
        body: changed ? JSON.stringify(merged) : null,
        link: linkFor(state.code, agent),
        now,
      });
      if (written.ok) {
        await closeAudit(env.DB, auditId, 200, null);
        const config = changed && written.version !== null ? { version: written.version, body: merged } : { version, body: base };
        return { config, attached };
      }
      if (attempt >= ATTEMPTS) throw new HttpError(409, 'attach_conflict');
      state = codeState(await findCodeById(env.DB, state.code.id), agent.id, now);
    }
  } catch (err) {
    await closeAudit(env.DB, auditId, err instanceof HttpError ? err.status : 500, err instanceof Error ? err.message : String(err));
    throw err;
  }
}

/** The audit row (`admin_audit`, actor `agent:<id>`), written before the change; no change goes unrecorded. */
async function openAudit(db: D1Database, agent: AgentRow, code: EnrollCodeRow, attached: string[], now: number): Promise<number> {
  try {
    return await insertAudit(db, {
      at: now,
      actor: `agent:${agent.id}`,
      method: 'POST',
      path: '/v1/attach',
      body: JSON.stringify({ code_id: code.id, minted_by: code.minted_by, attached }),
    });
  } catch (err) {
    logError('attach_audit_write_failed', err, { agent_id: agent.id });
    throw new HttpError(503, 'audit_unavailable');
  }
}

async function closeAudit(db: D1Database, id: number, status: number, error: string | null): Promise<void> {
  await finishAudit(db, id, status, error).catch((e: unknown) => logError('attach_audit_finish_failed', e, { id, status }));
}
