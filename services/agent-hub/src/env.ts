export interface Env {
  DB: D1Database;
  ARTIFACTS: R2Bucket;
  /** Bearer token for /admin (secret). */
  ADMIN_TOKEN: string;
  /** Base64 AES-256 key (32 bytes, secret) sealing bench bundles at rest; bench is off without it. */
  BENCH_KEY?: string;
  HEARTBEAT_SECONDS?: string;
  /** Heartbeat cadence cap for canary agents, the fast lane (default 15). */
  CANARY_HEARTBEAT_SECONDS?: string;
  GATE_MIN_AGENTS?: string;
  /** Fallback for GATE_SOAK_HOURS_PATCH (deployments from before the update lanes). */
  GATE_SOAK_HOURS?: string;
  /** Canary soak before a patch / minor / major may be promoted (defaults 2 / 24 / 72). */
  GATE_SOAK_HOURS_PATCH?: string;
  GATE_SOAK_HOURS_MINOR?: string;
  GATE_SOAK_HOURS_MAJOR?: string;
  /** Minor rollout ramp after promotion, "hours:percent,…" (default "0:10,24:50,48:100"). */
  MINOR_RAMP?: string;
}

export interface AgentRow {
  id: string;
  tenant_id: string;
  site_id: string;
  public_key: string;
  hostname: string;
  os: string;
  arch: string;
  version: string;
  channel_override: Channel | null;
  heartbeat_seconds: number;
  enrolled_at: number;
  revoked_at: number | null;
  last_seen_at: number | null;
}

export type Channel = 'canary' | 'stable';

export interface AppVars {
  agent: AgentRow;
  rawBody: string;
}

export type AppEnv = { Bindings: Env; Variables: AppVars };

export function intVar(value: string | undefined, fallback: number): number {
  const n = Number(value);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : fallback;
}

export function nowSeconds(): number {
  return Math.floor(Date.now() / 1000);
}
