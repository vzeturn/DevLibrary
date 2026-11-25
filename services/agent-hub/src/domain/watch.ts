// Site canary watch (RT-1106) as a pure function: sites on the canary list get a stable
// release first; their health decides whether it opens to the rest or is pulled back.

export interface WatchAgent {
  agent_id: string;
  /** Seconds since the agent's last heartbeat. */
  silent_seconds: number;
  /** The latest heartbeat runs the watched version. */
  on_version: boolean;
  /** The agent reported a rollback of this version. */
  rolled_back: boolean;
  /** Failed checks of the agent's latest self-test run on this version (0 when none ran). */
  selftest_failures: number;
  modules: ReadonlyArray<{ name: string; state: string }>;
}

export interface WatchInput {
  /** Hours the canary sites are watched before the release opens. */
  watchHours: number;
  /** Seconds since promotion. */
  elapsedSeconds: number;
  /** Silence longer than this on a site that took the release is a fault. */
  silentLimitSeconds: number;
  /** Site canary agents (enrolled on the canary list) of the product. */
  agents: ReadonlyArray<WatchAgent>;
  /** Error/critical fingerprints on the watched version that the previous version never had. */
  newErrors: ReadonlyArray<string>;
}

export type WatchVerdict = 'waiting' | 'good' | 'bad';

export interface WatchResult {
  verdict: WatchVerdict;
  /** Why the verdict is bad, or what the release still waits for. */
  reasons: string[];
}

const HEALTHY = new Set(['running', 'disabled']);

/**
 * `bad` as soon as one site that took the release shows a fault (rollback, silence, failed
 * self-test, module not running) or a new error appears; `good` once the watch time has
 * passed with at least one site on the new version and no fault (or when no site canary agent
 * is enrolled, so an empty lane never strands a release); otherwise `waiting`.
 */
export function evaluateWatch(input: WatchInput): WatchResult {
  const bad: string[] = [];
  for (const a of input.agents) {
    const tookIt = a.on_version || a.rolled_back;
    if (a.rolled_back) bad.push(`${a.agent_id}: rolled back`);
    if (a.on_version && a.silent_seconds > input.silentLimitSeconds) bad.push(`${a.agent_id}: silent ${a.silent_seconds}s`);
    if (tookIt && a.selftest_failures > 0) bad.push(`${a.agent_id}: self-test failed ${a.selftest_failures} check(s)`);
    if (a.on_version) {
      for (const m of a.modules) if (!HEALTHY.has(m.state)) bad.push(`${a.agent_id}/${m.name}: ${m.state}`);
    }
  }
  if (input.newErrors.length > 0) bad.push(`new error fingerprints: ${[...input.newErrors].sort().join(', ')}`);
  if (bad.length > 0) return { verdict: 'bad', reasons: bad };

  if (input.agents.length === 0) return { verdict: 'good', reasons: ['no site canary agent is enrolled for this product'] };
  const running = input.agents.filter((a) => a.on_version).length;
  if (input.elapsedSeconds < input.watchHours * 3600) {
    return { verdict: 'waiting', reasons: [`watching ${input.watchHours}h, ${Math.floor(input.elapsedSeconds / 3600)}h elapsed`] };
  }
  if (running === 0) return { verdict: 'waiting', reasons: ['no site canary runs this version yet'] };
  return { verdict: 'good', reasons: [`${running} site canary agent(s) healthy for ${input.watchHours}h`] };
}
