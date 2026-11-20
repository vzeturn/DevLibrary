import type { Env } from '../env';
import { sha256Hex } from '../lib/crypto';
import { findCodeByHash } from '../repo/enrollCodes';
import { latestSelftest } from '../repo/selftest';
import { agentsView } from './admin';
import { normaliseCode } from './enroll';
import type { SelftestSummary } from './selftest';

/**
 * What the deploy page (`/deploy`, RT-0292) shows the person holding a one-time code: whether a
 * machine used it yet, and then that machine's connection, modules and last self-test. Nothing
 * about other agents, the tenant's config or any secret.
 */
export type DeployState = 'unknown' | 'waiting' | 'expired' | 'revoked' | 'connected';

export interface DeployStatus {
  state: DeployState;
  product: string | null;
  site: string | null;
  expires_at: number | null;
  agent: {
    hostname: string;
    version: string;
    online: boolean;
    last_seen_at: number | null;
    modules: { name: string; state: string }[];
    selftest: (Pick<SelftestSummary, 'pass' | 'warn' | 'fail' | 'skip' | 'total'> & { finished_at: number }) | null;
  } | null;
}

const NONE: DeployStatus = { state: 'unknown', product: null, site: null, expires_at: null, agent: null };

export async function deployStatus(env: Env, rawCode: string, now: number): Promise<DeployStatus> {
  const code = await findCodeByHash(env.DB, await sha256Hex(normaliseCode(rawCode)));
  if (!code) return NONE;
  const base = { product: code.minted_by, site: code.site_id, expires_at: code.expires_at, agent: null };
  if (code.used_by_agent === null) {
    if (code.revoked_at !== null) return { ...base, state: 'revoked' };
    return { ...base, state: code.expires_at <= now ? 'expired' : 'waiting' };
  }
  const [agent] = await agentsView(env, undefined, now, code.used_by_agent);
  if (!agent) return { ...base, state: 'revoked' };
  const hb = agent.heartbeat as { modules?: { name?: unknown; state?: unknown }[] } | null;
  const modules = (hb?.modules ?? [])
    .filter((m) => typeof m.name === 'string' && typeof m.state === 'string' && m.name !== 'sys')
    .map((m) => ({ name: m.name as string, state: m.state as string }));
  const run = await latestSelftest(env.DB, agent.id);
  const summary = run ? (JSON.parse(run.summary_json) as SelftestSummary) : null;
  return {
    ...base,
    state: agent.revoked ? 'revoked' : 'connected',
    agent: {
      hostname: agent.hostname,
      version: agent.version,
      online: agent.online,
      last_seen_at: agent.last_seen_at,
      modules,
      selftest:
        run && summary
          ? { pass: summary.pass, warn: summary.warn, fail: summary.fail, skip: summary.skip, total: summary.total, finished_at: run.finished_at }
          : null,
    },
  };
}
