import { type AgentRow, type Channel, type Env } from '../env';
import { resolveChannel } from '../domain/channel';
import type { HeartbeatBody, ProblemReport } from '../domain/schemas';
import { compareVersions } from '../domain/version';
import { HttpError } from '../lib/http';
import { getTenantDemo } from '../repo/agents';
import { saveResult, takePending } from '../repo/commands';
import { getConfig } from '../repo/configs';
import { saveHeartbeat } from '../repo/heartbeats';
import { latestRelease } from '../repo/releases';
import { upsertReports } from '../repo/reports';
import type { ConfigView } from './config';

export interface WireCommand {
  id: string;
  kind: string;
  module?: string;
  args: unknown;
  expires_at: number;
}

export interface HeartbeatAnswer {
  server_time: number;
  channel: Channel;
  heartbeat_seconds: number;
  config?: ConfigView;
  commands: WireCommand[];
  update_check: boolean;
}

export async function agentChannel(db: D1Database, agent: AgentRow): Promise<Channel> {
  return resolveChannel(agent.channel_override, await getTenantDemo(db, agent.tenant_id));
}

export async function heartbeat(env: Env, agent: AgentRow, hb: HeartbeatBody, rawBody: string, now: number): Promise<HeartbeatAnswer> {
  if (hb.agent_id !== agent.id) throw new HttpError(400, 'agent_id_mismatch');
  const channel = await agentChannel(env.DB, agent);
  await saveHeartbeat(env.DB, agent.id, hb, channel, rawBody, now);

  const commands = (await takePending(env.DB, agent.id, now)).map((c) => ({
    id: c.id,
    kind: c.kind,
    ...(c.module ? { module: c.module } : {}),
    args: JSON.parse(c.args) as unknown,
    expires_at: c.expires_at,
  }));
  const config = await getConfig(env.DB, agent.id);
  const release = await latestRelease(env.DB, hb.product, channel);

  return {
    server_time: now,
    channel,
    heartbeat_seconds: agent.heartbeat_seconds,
    ...(config && config.version > hb.config_version
      ? { config: { version: config.version, body: JSON.parse(config.body) as unknown } }
      : {}),
    commands,
    update_check: release !== null && compareVersions(release.version, hb.version) > 0,
  };
}

export async function reports(env: Env, agent: AgentRow, items: ProblemReport[], now: number): Promise<{ accepted: number }> {
  await upsertReports(env.DB, agent.id, agent.tenant_id, items, now);
  return { accepted: items.length };
}

export async function commandResult(
  env: Env,
  agent: AgentRow,
  id: string,
  r: { ok: boolean; output: string; finished_at: number },
  now: number,
): Promise<Record<string, never>> {
  const outcome = await saveResult(env.DB, agent.id, id, r, now);
  if (outcome === 'not_found') throw new HttpError(404, 'command_not_found');
  if (outcome === 'duplicate') throw new HttpError(409, 'result_already_recorded');
  return {};
}
