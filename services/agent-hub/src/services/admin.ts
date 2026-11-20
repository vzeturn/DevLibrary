import type { Channel, Env } from '../env';
import { resolveChannel } from '../domain/channel';
import { randomCode, sha256Hex, uuidv7 } from '../lib/crypto';
import { HttpError } from '../lib/http';
import { getAgent, listAgents, revokeAgent, setChannelOverride } from '../repo/agents';
import { clearExpiredSecrets, insertCommand, listCommands, type CommandRow } from '../repo/commands';
import { insertCode } from '../repo/enrollCodes';
import { listProductLinks } from '../repo/productLinks';
import { ensureSite, getTenant } from '../repo/tenants';

export interface AgentView {
  id: string;
  tenant_id: string;
  site_id: string;
  hostname: string;
  os: string;
  arch: string;
  version: string;
  channel: Channel;
  channel_override: Channel | null;
  online: boolean;
  last_seen_at: number | null;
  degraded_modules: string[];
  revoked: boolean;
  heartbeat: unknown;
  /** Products this agent serves (`product_links`, RT-0298). */
  products: ProductLinkView[];
}

export interface ProductLinkView {
  product: string;
  external_id: string;
  created_at: number;
}

/** Offline after three missed heartbeats (PROTOCOL §3). */
export function isOnline(lastSeen: number | null, heartbeatSeconds: number, now: number): boolean {
  return lastSeen !== null && now - lastSeen <= 3 * heartbeatSeconds;
}

export async function agentsView(env: Env, tenantId: string | undefined, now: number, agentId?: string): Promise<AgentView[]> {
  const rows = await listAgents(env.DB, tenantId, agentId);
  const products = new Map<string, ProductLinkView[]>();
  for (const l of await listProductLinks(env.DB, { tenantId, agentId })) {
    const list = products.get(l.agent_id) ?? [];
    list.push({ product: l.product, external_id: l.external_id, created_at: l.created_at });
    products.set(l.agent_id, list);
  }
  return rows.map((a) => ({
    id: a.id,
    tenant_id: a.tenant_id,
    site_id: a.site_id,
    hostname: a.hostname,
    os: a.os,
    arch: a.arch,
    version: a.heartbeat_version ?? a.version,
    channel: resolveChannel(a.channel_override, a.tenant_demo === 1),
    channel_override: a.channel_override,
    online: a.revoked_at === null && isOnline(a.heartbeat_at, a.heartbeat_seconds, now),
    last_seen_at: a.heartbeat_at,
    degraded_modules: a.degraded_modules ? a.degraded_modules.split(', ') : [],
    revoked: a.revoked_at !== null,
    heartbeat: a.heartbeat_body ? (JSON.parse(a.heartbeat_body) as unknown) : null,
    products: products.get(a.id) ?? [],
  }));
}

/** One agent with its product links; 404 when unknown. */
export async function agentView(env: Env, agentId: string, now: number): Promise<AgentView> {
  const [agent] = await agentsView(env, undefined, now, agentId);
  if (!agent) throw new HttpError(404, 'agent_not_found');
  return agent;
}

export interface NewCode {
  tenant_id: string;
  site_id: string;
  ttl_seconds: number;
  /** Initial remote config for the agent that enrols with the code. */
  config?: Record<string, unknown>;
  /** Product whose key mints the code (absent: admin). */
  minted_by?: string;
}

export async function createEnrollCode(
  env: Env,
  req: NewCode,
  now: number,
): Promise<{ id: string; code: string; tenant_id: string; site_id: string; expires_at: number }> {
  if (!(await getTenant(env.DB, req.tenant_id))) throw new HttpError(404, 'tenant_not_found');
  await ensureSite(env.DB, req.tenant_id, req.site_id, now);
  const code = randomCode();
  const id = uuidv7(now * 1000);
  const expires_at = now + req.ttl_seconds;
  await insertCode(env.DB, {
    id,
    code_hash: await sha256Hex(code),
    tenant_id: req.tenant_id,
    site_id: req.site_id,
    created_at: now,
    expires_at,
    config: req.config ? JSON.stringify(req.config) : null,
    minted_by: req.minted_by ?? null,
  });
  return { id, code, tenant_id: req.tenant_id, site_id: req.site_id, expires_at };
}

export async function setChannel(env: Env, agentId: string, channel: Channel | null): Promise<void> {
  if (!(await setChannelOverride(env.DB, agentId, channel))) throw new HttpError(404, 'agent_not_found');
}

export async function revoke(env: Env, agentId: string, now: number): Promise<void> {
  if (!(await getAgent(env.DB, agentId))) throw new HttpError(404, 'agent_not_found');
  await revokeAgent(env.DB, agentId, now);
}

export async function enqueueCommand(
  env: Env,
  req: { agent_id: string; kind: string; module?: string; args: Record<string, unknown>; ttl_seconds: number },
  now: number,
): Promise<{ id: string; expires_at: number }> {
  const agent = await getAgent(env.DB, req.agent_id);
  if (!agent) throw new HttpError(404, 'agent_not_found');
  if (agent.revoked_at !== null) throw new HttpError(409, 'agent_revoked');
  const id = uuidv7(now * 1000);
  const expires_at = now + req.ttl_seconds;
  await insertCommand(env.DB, {
    id,
    agent_id: req.agent_id,
    kind: req.kind,
    module: req.module ?? null,
    args: JSON.stringify(req.args),
    created_at: now,
    expires_at,
  });
  return { id, expires_at };
}

export type CommandStatus = 'pending' | 'delivered' | 'done' | 'failed' | 'expired';

/** A command as admins see it: never its secret, only whether it still holds one. */
export type CommandView = Omit<CommandRow, 'secret'> & { status: CommandStatus; has_secret: boolean };

export async function commandsView(env: Env, agentId: string | undefined, now: number): Promise<CommandView[]> {
  await clearExpiredSecrets(env.DB, now);
  const rows = await listCommands(env.DB, agentId);
  return rows.map(({ secret, ...c }) => {
    let status: CommandStatus = 'pending';
    if (c.result_at !== null) status = c.result_ok === 1 ? 'done' : 'failed';
    else if (c.delivered_at !== null) status = 'delivered';
    else if (c.expires_at <= now) status = 'expired';
    return { ...c, status, has_secret: secret !== null };
  });
}
