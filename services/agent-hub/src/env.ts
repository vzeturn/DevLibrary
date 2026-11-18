export interface Env {
  DB: D1Database;
  ARTIFACTS: R2Bucket;
  /** Bearer token for /admin (secret). */
  ADMIN_TOKEN: string;
  HEARTBEAT_SECONDS?: string;
  GATE_MIN_AGENTS?: string;
  GATE_SOAK_HOURS?: string;
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

export const AGENT_PRODUCT = 'dl-agent';

export function intVar(value: string | undefined, fallback: number): number {
  const n = Number(value);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : fallback;
}

export function nowSeconds(): number {
  return Math.floor(Date.now() / 1000);
}
