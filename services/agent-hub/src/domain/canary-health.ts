// Canary lane health (RT-1054 part C) as a pure function: after a canary site receives a
// release, its heartbeat, selftest and error signals are watched for a window. Bad signals roll
// the lane back to the previous version and block promotion; good signals over the whole
// window allow it. The repo gathers facts, this decides.

export interface CanaryHealthConfig {
  /** Hours every canary agent must stay healthy on the version before promotion is allowed. */
  windowHours: number;
  /** Heartbeat intervals of silence that count as a fault. */
  maxMissedBeats: number;
  /** error/critical reports per hour (since install, hour floor 1) above which the build is bad. */
  maxErrorsPerHour: number;
}

export const DEFAULT_CANARY_HEALTH: CanaryHealthConfig = { windowHours: 4, maxMissedBeats: 5, maxErrorsPerHour: 5 };

export interface CanaryHealthAgent {
  agent_id: string;
  /** First heartbeat that carried the version. */
  installed_at: number;
  /** Latest heartbeat of the agent, any version. */
  last_heartbeat: number;
  heartbeat_seconds: number;
  /** The agent rolled the version back on its own. */
  rolled_back: boolean;
  /** Latest selftest run uploaded after the install; null when none yet. */
  selftest: { fail: number } | null;
  /** error/critical report occurrences on this version. */
  error_count: number;
}

export interface CanaryHealthInput {
  now: number;
  config: CanaryHealthConfig;
  agents: ReadonlyArray<CanaryHealthAgent>;
  /** Version the lane returns to on rollback (null when there is none). */
  previousVersion: string | null;
}

export type CanaryVerdict = 'healthy' | 'rollback' | 'pending';

export interface CanaryHealthResult {
  verdict: CanaryVerdict;
  /** Why the verdict is rollback (faults) or pending (what is still missing). */
  reasons: string[];
  rollbackTo: string | null;
  /** True only when the verdict is healthy. */
  promotable: boolean;
}

export function agentFaults(a: CanaryHealthAgent, now: number, cfg: CanaryHealthConfig): string[] {
  const faults: string[] = [];
  const interval = Math.max(a.heartbeat_seconds, 1);
  const missed = Math.floor(Math.max(now - a.last_heartbeat, 0) / interval);
  if (missed >= cfg.maxMissedBeats) faults.push(`${a.agent_id}: ${missed} heartbeats missed`);
  if (a.selftest && a.selftest.fail > 0) faults.push(`${a.agent_id}: selftest failed (${a.selftest.fail} checks)`);
  const hours = Math.max((now - a.installed_at) / 3600, 1);
  const rate = a.error_count / hours;
  if (rate > cfg.maxErrorsPerHour) faults.push(`${a.agent_id}: error rate ${rate.toFixed(1)}/h over ${cfg.maxErrorsPerHour}/h`);
  if (a.rolled_back) faults.push(`${a.agent_id}: agent rolled the release back`);
  return faults;
}

export function evaluateCanaryHealth(input: CanaryHealthInput): CanaryHealthResult {
  const { now, config, agents } = input;
  const faults = agents.flatMap((a) => agentFaults(a, now, config));
  // A fault never waits for the window: a bad build must leave the lane at once.
  if (faults.length > 0) return { verdict: 'rollback', reasons: faults, rollbackTo: input.previousVersion, promotable: false };

  const windowSeconds = config.windowHours * 3600;
  const waiting = agents.filter((a) => now - a.installed_at < windowSeconds);
  if (agents.length === 0) return { verdict: 'pending', reasons: ['no canary agent has the release yet'], rollbackTo: null, promotable: false };
  if (waiting.length > 0) {
    const reasons = waiting.map((a) => `${a.agent_id}: ${((now - a.installed_at) / 3600).toFixed(1)}h of ${config.windowHours}h observed`);
    return { verdict: 'pending', reasons, rollbackTo: null, promotable: false };
  }
  return { verdict: 'healthy', reasons: [], rollbackTo: null, promotable: true };
}
