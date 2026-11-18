import type { Channel } from '../env';

/** PROTOCOL §5: an explicit override wins; otherwise demo tenants are canary, everyone else stable. */
export function resolveChannel(override: Channel | null, tenantDemo: boolean): Channel {
  if (override) return override;
  return tenantDemo ? 'canary' : 'stable';
}
