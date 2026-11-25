// Kill switch (PROTOCOL §5.3): pause a release, hold a tenant or one site.
import type { AgentRow, Env } from '../env';
import { HttpError } from '../lib/http';
import { ANY, addHold, addPause, getPause, isHeld, listHolds, removeHold, removePause, type HoldRow, type PauseRow } from '../repo/holds';
import { getRelease } from '../repo/releases';
import { clearWatch } from '../repo/watch';
import { getTenant } from '../repo/tenants';

/** How long after delivery an `update_now` keeps the manifest open to a held agent. */
export const UPDATE_NOW_WINDOW_SECONDS = 600;

async function requireRelease(env: Env, product: string, version: string): Promise<void> {
  const known = (await getRelease(env.DB, product, version, 'canary')) ?? (await getRelease(env.DB, product, version, 'stable'));
  if (!known) throw new HttpError(404, 'release_not_found');
}

export async function pauseRelease(env: Env, product: string, version: string, reason: string | undefined, now: number): Promise<PauseRow> {
  await requireRelease(env, product, version);
  await addPause(env.DB, product, version, reason ?? null, now);
  return (await getPause(env.DB, product, version)) as PauseRow;
}

export async function resumeRelease(env: Env, product: string, version: string): Promise<void> {
  if (!(await removePause(env.DB, product, version))) throw new HttpError(404, 'release_not_paused');
  await clearWatch(env.DB, product, version);
}

export const releasePause = (env: Env, product: string, version: string): Promise<PauseRow | null> => getPause(env.DB, product, version);

export interface HoldTarget {
  site_id?: string | undefined;
  product?: string | undefined;
  reason?: string | undefined;
}

export async function holdTenant(env: Env, tenantId: string, t: HoldTarget, now: number): Promise<HoldRow> {
  if (!(await getTenant(env.DB, tenantId))) throw new HttpError(404, 'tenant_not_found');
  const row = { tenant_id: tenantId, site_id: t.site_id ?? ANY, product: t.product ?? ANY, reason: t.reason ?? null };
  await addHold(env.DB, row, now);
  return { ...row, created_at: now };
}

export async function releaseHold(env: Env, tenantId: string, t: HoldTarget): Promise<void> {
  if (!(await removeHold(env.DB, tenantId, t.site_id ?? ANY, t.product ?? ANY))) throw new HttpError(404, 'hold_not_found');
}

export const holdsView = (env: Env, tenantId?: string): Promise<HoldRow[]> => listHolds(env.DB, tenantId);

export const agentHeld = (env: Env, agent: AgentRow, product: string): Promise<boolean> =>
  isHeld(env.DB, agent.tenant_id, agent.site_id, product);

/** An `update_now` handed to the agent and not yet answered: the operator's override of a hold. */
export async function updateNowActive(env: Env, agentId: string, now: number): Promise<boolean> {
  const row = await env.DB
    .prepare(
      `SELECT 1 AS ok FROM commands WHERE agent_id = ?1 AND kind = 'update_now'
       AND delivered_at IS NOT NULL AND delivered_at >= ?2 AND result_at IS NULL LIMIT 1`,
    )
    .bind(agentId, now - UPDATE_NOW_WINDOW_SECONDS)
    .first<{ ok: number }>();
  return row !== null;
}
