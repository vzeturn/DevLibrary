// Update lanes (PROTOCOL §5.1–5.2, docs/decisions/0001-update-lanes.md) as pure functions:
// how far a stable release moves, which gate it needs and how many stable agents get it now.

/** The semver step from the current stable to a promoted release; `initial` when there is no stable yet. */
export type Bump = 'major' | 'minor' | 'patch' | 'initial';

/** Gate settings that do not depend on the bump. */
export interface LaneConfig {
  minAgents: number;
  soakHoursPatch: number;
  soakHoursMinor: number;
  soakHoursMajor: number;
  ramp: ReadonlyArray<RampStep>;
}

/** From `afterHours` after promotion, `percent` of stable agents are offered a minor release. */
export interface RampStep {
  afterHours: number;
  percent: number;
}

/** A major needs at least this many canary agents whatever GATE_MIN_AGENTS says. */
export const MAJOR_MIN_AGENTS = 2;

export const DEFAULT_RAMP: ReadonlyArray<RampStep> = [
  { afterHours: 0, percent: 10 },
  { afterHours: 24, percent: 50 },
  { afterHours: 48, percent: 100 },
];

function core(version: string): number[] {
  const [main = ''] = version.split('-', 1);
  return main.split('.').map((p) => Number.parseInt(p, 10) || 0);
}

export function bumpOf(currentStable: string | null, next: string): Bump {
  if (currentStable === null) return 'initial';
  const [a, b] = [core(currentStable), core(next)];
  if ((a[0] ?? 0) !== (b[0] ?? 0)) return 'major';
  if ((a[1] ?? 0) !== (b[1] ?? 0)) return 'minor';
  return 'patch';
}

export function gateRules(cfg: LaneConfig, bump: Bump): { soakHours: number; minAgents: number } {
  switch (bump) {
    case 'major':
      return { soakHours: cfg.soakHoursMajor, minAgents: Math.max(cfg.minAgents, MAJOR_MIN_AGENTS) };
    case 'minor':
      return { soakHours: cfg.soakHoursMinor, minAgents: cfg.minAgents };
    default:
      return { soakHours: cfg.soakHoursPatch, minAgents: cfg.minAgents };
  }
}

/**
 * Parses `"hours:percent,…"` (e.g. `"0:10,24:50,48:100"`). Hours must rise, percents must not
 * fall and the last step must reach 100 (a ramp that never finishes would strand agents);
 * anything else falls back to {@link DEFAULT_RAMP}.
 */
export function parseRamp(text: string | undefined): ReadonlyArray<RampStep> {
  if (!text?.trim()) return DEFAULT_RAMP;
  const steps: RampStep[] = [];
  for (const part of text.split(',')) {
    const m = /^\s*(\d{1,4}):(\d{1,3})\s*$/.exec(part);
    if (!m) return DEFAULT_RAMP;
    const step = { afterHours: Number(m[1]), percent: Number(m[2]) };
    const prev = steps.at(-1);
    if (step.percent > 100 || (prev && (step.afterHours <= prev.afterHours || step.percent < prev.percent))) return DEFAULT_RAMP;
    steps.push(step);
  }
  return steps.at(-1)?.percent === 100 ? steps : DEFAULT_RAMP;
}

/** The ramp's percent `elapsedSeconds` after promotion (0 before its first step). */
export function rampPercent(steps: ReadonlyArray<RampStep>, elapsedSeconds: number): number {
  let pct = 0;
  for (const s of steps) if (elapsedSeconds >= s.afterHours * 3600) pct = s.percent;
  return pct;
}

/**
 * Share of stable agents offered a promoted release now, capped by the signed manifest's
 * `rollout_percent`; `null` for a major, which only opted-in tenants and sites get.
 */
export function offeredPercent(bump: Bump, ramp: ReadonlyArray<RampStep>, elapsedSeconds: number, ceiling: number): number | null {
  if (bump === 'major') return null;
  const lane = bump === 'minor' ? rampPercent(ramp, elapsedSeconds) : 100;
  return Math.min(lane, ceiling);
}

/**
 * The agent's bucket 0–99 for one release, exactly as dl-agent computes it (`channel.rs`
 * `rollout_bucket`): `sha256(agent_id|product|version)[0..2]` big-endian, mod 100.
 */
export async function rolloutBucket(agentId: string, product: string, version: string): Promise<number> {
  const h = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`${agentId}|${product}|${version}`)));
  return (((h[0] ?? 0) << 8) | (h[1] ?? 0)) % 100;
}

export async function inRollout(agentId: string, product: string, version: string, percent: number): Promise<boolean> {
  return (await rolloutBucket(agentId, product, version)) < Math.min(percent, 100);
}
