import { intVar, type Env } from '../env';
import { hubUrls } from '../domain/hubUrls';
import { resolveChannel } from '../domain/channel';
import type { EnrollBody } from '../domain/schemas';
import { fromBase64, importEd25519, sha256Hex, uuidv7 } from '../lib/crypto';
import { HttpError } from '../lib/http';
import { getTenantDemo } from '../repo/agents';
import { claimCodeAndCreateAgent, findCodeByHash } from '../repo/enrollCodes';
import { configView, type ConfigView } from './config';

export interface EnrollAnswer {
  agent_id: string;
  tenant: string;
  site: string;
  channel: string;
  heartbeat_seconds: number;
  config: ConfigView;
  /** Hub hosts, primary first (PROTOCOL §2.1); absent when HUB_URLS is not set. */
  hub_urls?: string[];
}

/** Codes are typed by people: case and surrounding spaces do not matter. */
export function normaliseCode(code: string): string {
  return code.trim().toUpperCase();
}

export async function enroll(env: Env, body: EnrollBody, now: number): Promise<EnrollAnswer> {
  const pk = fromBase64(body.public_key);
  if (!pk || !(await importEd25519(pk))) throw new HttpError(400, 'invalid_public_key');

  const code = await findCodeByHash(env.DB, await sha256Hex(normaliseCode(body.code)));
  if (!code || code.used_at !== null || code.revoked_at !== null || code.expires_at <= now) {
    throw new HttpError(403, 'enroll_code_invalid');
  }

  const heartbeatSeconds = intVar(env.HEARTBEAT_SECONDS, 60);
  const agentId = uuidv7(now * 1000);
  const claimed = await claimCodeAndCreateAgent(
    env.DB,
    code.id,
    {
      id: agentId,
      public_key: body.public_key,
      hostname: body.hostname,
      os: body.os,
      arch: body.arch,
      version: body.version,
      modules: body.modules,
      heartbeat_seconds: heartbeatSeconds,
    },
    now,
  );
  if (!claimed) throw new HttpError(403, 'enroll_code_invalid');

  const demo = await getTenantDemo(env.DB, code.tenant_id);
  return {
    agent_id: agentId,
    tenant: code.tenant_id,
    site: code.site_id,
    channel: resolveChannel(null, demo),
    heartbeat_seconds: heartbeatSeconds,
    config: await configView(env.DB, agentId),
    ...withHubUrls(env),
  };
}

/** `{ hub_urls }` for an answer, or nothing when the hub has no list configured. */
export function withHubUrls(env: Env): { hub_urls?: string[] } {
  const urls = hubUrls(env.HUB_URLS);
  return urls.length > 0 ? { hub_urls: urls } : {};
}
