import type { MiddlewareHandler } from 'hono';
import { nowSeconds, type AppEnv } from '../env';
import { sha256Hex, verifyEd25519 } from '../lib/crypto';
import { HttpError } from '../lib/http';
import { getAgent, recordNonce } from '../repo/agents';

export const MAX_SKEW_SECONDS = 300;
export const NONCE_TTL_SECONDS = 600;

/** PROTOCOL §1 canonical string. */
export function canonicalString(method: string, pathAndQuery: string, time: string, nonce: string, bodySha256Hex: string): string {
  return `DLAGENT1\n${method}\n${pathAndQuery}\n${time}\n${nonce}\n${bodySha256Hex}`;
}

/** Verifies the agent signature and exposes the agent and raw body to the handler. */
export const requireSignedAgent: MiddlewareHandler<AppEnv> = async (c, next) => {
  const agentId = c.req.header('X-Agent-Id');
  const time = c.req.header('X-Agent-Time');
  const nonce = c.req.header('X-Agent-Nonce');
  const signature = c.req.header('X-Agent-Signature');
  if (!agentId || !time || !nonce || !signature) throw new HttpError(401, 'unsigned');
  if (!/^\d{1,12}$/.test(time) || !/^[0-9a-f]{32}$/.test(nonce)) throw new HttpError(401, 'bad_headers');

  const agent = await getAgent(c.env.DB, agentId);
  if (!agent) throw new HttpError(401, 'unknown_agent');
  if (agent.revoked_at !== null) throw new HttpError(401, 'revoked');

  const now = nowSeconds();
  if (Math.abs(now - Number(time)) > MAX_SKEW_SECONDS) throw new HttpError(401, 'clock_skew', { server_time: now });

  const body = new Uint8Array(await c.req.arrayBuffer());
  const url = new URL(c.req.url);
  const canonical = canonicalString(c.req.method, url.pathname + url.search, time, nonce, await sha256Hex(body));
  if (!(await verifyEd25519(agent.public_key, signature, canonical))) throw new HttpError(401, 'bad_signature');

  if (!(await recordNonce(c.env.DB, agent.id, nonce, now, NONCE_TTL_SECONDS))) throw new HttpError(401, 'replay');

  c.set('agent', agent);
  c.set('rawBody', new TextDecoder().decode(body));
  await next();
};
