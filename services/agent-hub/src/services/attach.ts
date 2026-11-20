import type { AgentRow, Env } from '../env';
import type { AttachBody } from '../domain/schemas';
import { sha256Hex } from '../lib/crypto';
import { HttpError } from '../lib/http';
import { claimCodeForAttach, findCodeByHash } from '../repo/enrollCodes';
import { configView, DEFAULT_CONFIG, type ConfigView } from './config';
import { normaliseCode } from './enroll';

export interface AttachAnswer {
  agent_id: string;
  /** The agent's own tenant and site: attaching never moves the agent. */
  tenant: string;
  site: string;
  /** Product whose key minted the code; null for an admin code. */
  product: string | null;
  /** The product's tenant the agent now also serves (the code's tenant). */
  product_tenant: string;
  /** Modules the code switched on. */
  modules: string[];
  config: ConfigView;
}

/** `modules` of a code's initial config, or `{}` when the code carries none. */
export function codeModules(config: string | null): Record<string, unknown> {
  if (config === null) return {};
  const parsed = JSON.parse(config) as { modules?: unknown };
  const modules = parsed.modules;
  return modules && typeof modules === 'object' && !Array.isArray(modules) ? (modules as Record<string, unknown>) : {};
}

/**
 * Attaches one more product to an enrolled agent (RT-0298) with a one-time code: the code is
 * spent, its modules are merged into the agent's config (other modules and settings stay) and
 * the product link is recorded. The agent keeps its id, key, tenant, site and channel; the new
 * config reaches it on the next heartbeat.
 */
export async function attach(env: Env, agent: AgentRow, body: AttachBody, now: number): Promise<AttachAnswer> {
  const code = await findCodeByHash(env.DB, await sha256Hex(normaliseCode(body.code)));
  if (!code || code.used_at !== null || code.revoked_at !== null || code.expires_at <= now) {
    throw new HttpError(403, 'enroll_code_invalid');
  }
  // An admin code names a tenant, not a product: it may only add modules inside the agent's own.
  if (code.minted_by === null && code.tenant_id !== agent.tenant_id) {
    throw new HttpError(409, 'tenant_mismatch', { agent_tenant: agent.tenant_id, code_tenant: code.tenant_id });
  }

  const modules = codeModules(code.config);
  const patch = JSON.stringify({ modules });
  const claimed = await claimCodeForAttach(env.DB, code.id, agent.id, patch, JSON.stringify(DEFAULT_CONFIG.body), now);
  if (!claimed) throw new HttpError(403, 'enroll_code_invalid');

  return {
    agent_id: agent.id,
    tenant: agent.tenant_id,
    site: agent.site_id,
    product: code.minted_by,
    product_tenant: code.tenant_id,
    modules: Object.keys(modules).sort(),
    config: await configView(env.DB, agent.id),
  };
}
