import { getConfig, putConfig } from '../repo/configs';
import { HttpError } from '../lib/http';
import { getAgent } from '../repo/agents';

export interface ConfigView {
  version: number;
  body: unknown;
}

/** Version 0 is the built-in default an agent starts with before any config was set. */
export const DEFAULT_CONFIG: ConfigView = { version: 0, body: { modules: {}, log_level: 'info' } };

export async function configView(db: D1Database, agentId: string): Promise<ConfigView> {
  const row = await getConfig(db, agentId);
  return row ? { version: row.version, body: JSON.parse(row.body) as unknown } : DEFAULT_CONFIG;
}

export async function setConfig(db: D1Database, agentId: string, body: Record<string, unknown>, now: number): Promise<ConfigView> {
  if (!(await getAgent(db, agentId))) throw new HttpError(404, 'agent_not_found');
  const version = await putConfig(db, agentId, JSON.stringify(body), now);
  return { version, body };
}
