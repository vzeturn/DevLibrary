// Canary gate (PROTOCOL §5) as a pure function: the repo gathers facts, this decides.
import type { CanaryHealthResult } from './canary-health';

export type GateCheckName = 'min_agents' | 'soak' | 'modules_running' | 'no_new_errors' | 'no_rollback' | 'site_eligible' | 'canary_health';

export interface GateAgent {
  agent_id: string;
  /** First and last heartbeat that carried the release version. */
  first_seen: number;
  last_seen: number;
  rolled_back: boolean;
  /** The agent's latest heartbeat still runs this version. */
  on_version: boolean;
  modules: ReadonlyArray<{ name: string; state: string }>;
}

export interface GateInput {
  minAgents: number;
  soakHours: number;
  /** Canary agents that reported the release version. */
  agents: ReadonlyArray<GateAgent>;
  /** Fingerprints reported on the release version with severity error or critical. */
  errorFingerprints: ReadonlyArray<string>;
  /** Every fingerprint seen on the previous version (null when there is none). */
  previousVersion: string | null;
  previousFingerprints: ReadonlyArray<string>;
  /**
   * Canary sites that do not meet the product's site requirements (§9.5), each with its
   * reasons; `null` when the product has none. A canary on an unready site proves nothing.
   */
  unreadySites?: ReadonlyArray<{ agent_id: string; reasons: ReadonlyArray<string> }> | null;
  /** Canary health verdict (RT-1054); promotion needs `healthy`. Omitted: the check is not run. */
  canaryHealth?: CanaryHealthResult | undefined;
}

export interface GateCheck {
  name: GateCheckName;
  ok: boolean;
  detail: string;
}

export interface GateResult {
  pass: boolean;
  checks: GateCheck[];
}

/** Module states that do not block promotion: `disabled` is an operator choice, not a fault. */
const HEALTHY = new Set(['running', 'disabled']);

export function evaluateGate(input: GateInput): GateResult {
  const soakSeconds = input.soakHours * 3600;
  const live = input.agents.filter((a) => !a.rolled_back);
  const soaked = live.filter((a) => a.last_seen - a.first_seen >= soakSeconds);

  const unhealthy = input.agents
    .filter((a) => a.on_version)
    .flatMap((a) => a.modules.filter((m) => !HEALTHY.has(m.state)).map((m) => `${a.agent_id}/${m.name}=${m.state}`));

  const known = new Set(input.previousFingerprints);
  const fresh = [...new Set(input.errorFingerprints)].filter((f) => !known.has(f)).sort();
  const rolledBack = input.agents.filter((a) => a.rolled_back).map((a) => a.agent_id);

  const checks: GateCheck[] = [
    {
      name: 'min_agents',
      ok: live.length >= input.minAgents,
      detail: `${live.length}/${input.minAgents} canary agents on this version`,
    },
    {
      name: 'soak',
      ok: soaked.length >= input.minAgents,
      detail: `${soaked.length}/${input.minAgents} agents soaked ${input.soakHours}h`,
    },
    {
      name: 'modules_running',
      ok: unhealthy.length === 0,
      detail: unhealthy.length === 0 ? 'all modules running' : `not running: ${unhealthy.join(', ')}`,
    },
    {
      name: 'no_new_errors',
      ok: fresh.length === 0,
      detail:
        fresh.length === 0
          ? `no new error/critical fingerprints vs ${input.previousVersion ?? '(none)'}`
          : `new vs ${input.previousVersion ?? '(none)'}: ${fresh.join(', ')}`,
    },
    {
      name: 'no_rollback',
      ok: rolledBack.length === 0,
      detail: rolledBack.length === 0 ? 'no rollbacks' : `rolled back on: ${rolledBack.join(', ')}`,
    },
    {
      name: 'site_eligible',
      ok: !input.unreadySites?.length,
      detail:
        input.unreadySites == null
          ? 'no site requirements for this product'
          : input.unreadySites.length === 0
            ? 'every canary site meets the site requirements'
            : `not ready: ${input.unreadySites.map((s) => `${s.agent_id} (${s.reasons.join('; ')})`).join(', ')}`,
    },
  ];
  if (input.canaryHealth) {
    const h = input.canaryHealth;
    checks.push({
      name: 'canary_health',
      ok: h.promotable,
      detail: h.promotable ? 'canary signals healthy for the whole window' : `${h.verdict}: ${h.reasons.join('; ')}`,
    });
  }
  return { pass: checks.every((c) => c.ok), checks };
}
