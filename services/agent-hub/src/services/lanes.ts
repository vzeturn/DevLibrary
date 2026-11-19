// Update lanes (PROTOCOL §5.1–5.2, docs/decisions/0001-update-lanes.md): canary is the fast
// lane for test machines; stable releases roll out by their semver bump.
import { intVar, type AgentRow, type Channel, type Env } from '../env';
import { bumpOf, inRollout, offeredPercent, parseRamp, type Bump, type LaneConfig, type RampStep } from '../domain/lanes';
import type { HeartbeatBody } from '../domain/schemas';
import { HttpError, logError } from '../lib/http';
import { addAllow, ALL_SITES, currentStable, isAllowed, latestPromotion, listAllows, removeAllow, type AllowRow } from '../repo/lanes';
import { getRelease, type ReleaseRow } from '../repo/releases';
import { getTenant } from '../repo/tenants';

/** Canary heartbeat cadence cap when CANARY_HEARTBEAT_SECONDS is unset. */
export const DEFAULT_CANARY_HEARTBEAT_SECONDS = 15;
/** Cadence of a canary agent on probation, so its commit or rollback shows on the hub at once. */
export const PROBATION_HEARTBEAT_SECONDS = 5;

export function laneConfig(env: Env): LaneConfig {
  return {
    minAgents: intVar(env.GATE_MIN_AGENTS, 1),
    soakHoursPatch: intVar(env.GATE_SOAK_HOURS_PATCH ?? env.GATE_SOAK_HOURS, 2),
    soakHoursMinor: intVar(env.GATE_SOAK_HOURS_MINOR, 24),
    soakHoursMajor: intVar(env.GATE_SOAK_HOURS_MAJOR, 72),
    ramp: parseRamp(env.MINOR_RAMP),
  };
}

/** Heartbeat cadence: canary agents (test machines) beat fast, faster still while on probation. */
export function heartbeatSeconds(env: Env, agent: AgentRow, channel: Channel, hb: HeartbeatBody): number {
  if (channel !== 'canary') return agent.heartbeat_seconds;
  const fast = Math.min(agent.heartbeat_seconds, intVar(env.CANARY_HEARTBEAT_SECONDS, DEFAULT_CANARY_HEARTBEAT_SECONDS));
  return hb.update?.state === 'probation' ? Math.min(fast, PROBATION_HEARTBEAT_SECONDS) : fast;
}

/**
 * The signed manifest's `rollout_percent`, the ceiling of every lane. The text passed the
 * manifest schema when it was stored; should it no longer parse, the hub offers the release
 * and leaves the cut to the agent, which applies the same percentage itself.
 */
function manifestCeiling(row: ReleaseRow): number {
  let value: unknown;
  try {
    value = (JSON.parse(row.manifest) as { rollout_percent?: unknown }).rollout_percent;
  } catch (error) {
    logError('stored_manifest_unreadable', error, { product: row.product, version: row.version, channel: row.channel });
    return 100;
  }
  if (typeof value === 'number' && value >= 0 && value <= 100) return value;
  logError('stored_manifest_rollout_invalid', value, { product: row.product, version: row.version, channel: row.channel });
  return 100;
}

/**
 * Whether a stable agent is offered `release` now: a patch to every agent, a minor to the
 * ramp's cohort (the agent's own rollout bucket), a major only where an admin opted in.
 * Promotions from before the lanes (no bump) behave as a patch, as they did.
 */
export async function stableOffered(env: Env, agent: AgentRow, release: ReleaseRow, now: number): Promise<boolean> {
  const promotion = await latestPromotion(env.DB, release.product, release.version);
  const bump = promotion?.bump ?? 'patch';
  const ceiling = manifestCeiling(release);
  if (bump === 'major') {
    const allowed = await isAllowed(env.DB, release.product, release.version, agent.tenant_id, agent.site_id);
    return allowed && inRollout(agent.id, release.product, release.version, ceiling);
  }
  const elapsed = now - (promotion?.promoted_at ?? now);
  const pct = offeredPercent(bump, laneConfig(env).ramp, elapsed, ceiling) ?? 0;
  return inRollout(agent.id, release.product, release.version, pct);
}

export interface LaneStatus {
  bump: Bump;
  /** The stable release the bump is measured against. */
  current_stable: string | null;
  promoted_at: number | null;
  /** Share of stable agents offered the release now; null before promotion and for a major. */
  rollout_percent: number | null;
  /** The minor ramp (hours after promotion → percent); null for other bumps. */
  ramp: ReadonlyArray<RampStep> | null;
  /** A major is never offered on its own: only to `allowed` tenants/sites, or by update_now. */
  automatic: boolean;
  allowed: AllowRow[];
}

/** The lane of a release: its bump (as recorded at promotion, else measured now) and rollout. */
export async function releaseLane(env: Env, product: string, version: string, now: number): Promise<LaneStatus> {
  const [promotion, current, stable, allowed] = await Promise.all([
    latestPromotion(env.DB, product, version),
    currentStable(env.DB, product, version),
    getRelease(env.DB, product, version, 'stable'),
    listAllows(env.DB, product, version),
  ]);
  const bump = promotion?.bump ?? bumpOf(current, version);
  const cfg = laneConfig(env);
  const rollout = promotion && stable ? offeredPercent(bump, cfg.ramp, now - promotion.promoted_at, manifestCeiling(stable)) : null;
  return {
    bump,
    current_stable: current,
    promoted_at: promotion?.promoted_at ?? null,
    rollout_percent: rollout,
    ramp: bump === 'minor' ? cfg.ramp : null,
    automatic: bump !== 'major',
    allowed,
  };
}

export interface AllowTarget {
  tenant_id: string;
  site_id?: string | undefined;
}

/** Opts a tenant (every site) or one of its sites in to a major release. */
export async function allowRelease(env: Env, product: string, version: string, t: AllowTarget, now: number): Promise<AllowRow> {
  if (!(await getRelease(env.DB, product, version, 'canary'))) throw new HttpError(404, 'release_not_found');
  if (!(await getTenant(env.DB, t.tenant_id))) throw new HttpError(404, 'tenant_not_found');
  const siteId = t.site_id ?? ALL_SITES;
  await addAllow(env.DB, product, version, t.tenant_id, siteId, now);
  return { tenant_id: t.tenant_id, site_id: siteId, created_at: now };
}

export async function disallowRelease(env: Env, product: string, version: string, t: AllowTarget): Promise<void> {
  if (!(await removeAllow(env.DB, product, version, t.tenant_id, t.site_id ?? ALL_SITES))) throw new HttpError(404, 'allow_not_found');
}
