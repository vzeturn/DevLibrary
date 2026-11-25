// Site canary lane (RT-1106): stable releases reach the listed sites first, are watched for
// SITE_CANARY_HOURS, and then open to the ramp or are paused (the previous stable is served again).
import { intVar, type AgentRow, type Env } from '../env';
import { evaluateWatch, type WatchResult } from '../domain/watch';
import { HttpError } from '../lib/http';
import { getTenant } from '../repo/tenants';
import { fingerprintsFor, previousVersion, getRelease, type ReleaseRow } from '../repo/releases';
import { addSiteCanary, ANY_SITE, getWatch, isSiteCanary, newerPulledBack, laneStartedAt, listSiteCanaries, putWatch, removeSiteCanary, watchAgents, type SiteCanaryRow } from '../repo/watch';
import { addPause } from '../repo/holds';

export const DEFAULT_SITE_CANARY_HOURS = 4;
/** A site that took the release and stays silent this long is a fault. */
export const DEFAULT_SILENT_LIMIT_SECONDS = 900;

export interface WatchConfig {
  hours: number;
  silentLimitSeconds: number;
}

export function watchConfig(env: Env): WatchConfig {
  return {
    hours: intVar(env.SITE_CANARY_HOURS, DEFAULT_SITE_CANARY_HOURS),
    silentLimitSeconds: intVar(env.SITE_CANARY_SILENT_SECONDS, DEFAULT_SILENT_LIMIT_SECONDS),
  };
}

async function judge(env: Env, release: ReleaseRow, promotedAt: number, now: number): Promise<WatchResult> {
  const cfg = watchConfig(env);
  const previous = await previousVersion(env.DB, release.product, release.version, release.published_at);
  const [agents, errors, known] = await Promise.all([
    watchAgents(env.DB, release.product, release.version, now),
    fingerprintsFor(env.DB, release.version, true),
    previous ? fingerprintsFor(env.DB, previous, false) : Promise.resolve([] as string[]),
  ]);
  const seen = new Set(known);
  return evaluateWatch({
    watchHours: cfg.hours,
    elapsedSeconds: now - promotedAt,
    silentLimitSeconds: cfg.silentLimitSeconds,
    agents,
    newErrors: [...new Set(errors)].filter((f) => !seen.has(f)),
  });
}

/**
 * What the site canary lane says about offering `release` (promoted at `promotedAt`) to `agent`:
 * `open` = no lane applies or the watch passed (the normal ramp decides), `offer` = a site
 * canary takes it now, `hold` = still being watched or pulled back.
 * A bad verdict pauses the release, so the manifest endpoint serves the previous stable again.
 */
export async function siteCanaryGate(env: Env, agent: AgentRow, release: ReleaseRow, promotedAt: number, now: number): Promise<'open' | 'offer' | 'hold'> {
  const started = await laneStartedAt(env.DB);
  if (started === null || promotedAt < started) return 'open';
  if (await newerPulledBack(env.DB, release.product, release.version, promotedAt)) return 'open';
  const verdict = await getWatch(env.DB, release.product, release.version, promotedAt);
  if (verdict?.verdict === 'good') return 'open';
  if (verdict?.verdict === 'bad') return 'hold';
  const isCanary = await isSiteCanary(env.DB, agent.tenant_id, agent.site_id);
  const result = await judge(env, release, promotedAt, now);
  if (result.verdict !== 'waiting') {
    await putWatch(env.DB, release.product, release.version, promotedAt, result.verdict, result.reasons, now);
    // The first stored verdict wins: a concurrent heartbeat that judged differently follows it.
    const stored = await getWatch(env.DB, release.product, release.version, promotedAt);
    if (stored?.verdict === 'bad') {
      await addPause(env.DB, release.product, release.version, `site canary failed: ${(JSON.parse(stored.reasons) as string[]).join('; ').slice(0, 400)}`, now);
      return 'hold';
    }
    return 'open';
  }
  return isCanary ? 'offer' : 'hold';
}

export async function addCanarySite(env: Env, tenantId: string, siteId: string | undefined, now: number): Promise<SiteCanaryRow> {
  if (!(await getTenant(env.DB, tenantId))) throw new HttpError(404, 'tenant_not_found');
  const site = siteId ?? ANY_SITE;
  await addSiteCanary(env.DB, tenantId, site, now);
  return { tenant_id: tenantId, site_id: site, created_at: now };
}

export async function removeCanarySite(env: Env, tenantId: string, siteId: string | undefined): Promise<void> {
  if (!(await removeSiteCanary(env.DB, tenantId, siteId ?? ANY_SITE))) throw new HttpError(404, 'site_canary_not_found');
}

export const canarySites = (env: Env): Promise<SiteCanaryRow[]> => listSiteCanaries(env.DB);

/** The recorded watch verdict of a promoted release, for the gate view. */
export async function watchStatus(env: Env, product: string, version: string, promotedAt: number | null): Promise<{ verdict: string; reasons: string[] } | null> {
  if (promotedAt === null || !(await getRelease(env.DB, product, version, 'stable'))) return null;
  const row = await getWatch(env.DB, product, version, promotedAt);
  return row ? { verdict: row.verdict, reasons: JSON.parse(row.reasons) as string[] } : { verdict: 'waiting', reasons: [] };
}
