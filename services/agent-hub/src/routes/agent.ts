import { Hono } from 'hono';
import { nowSeconds, type AppEnv } from '../env';
import { commandResultBody, enrollBody, heartbeatBody, MAX_SELFTEST_BYTES, reportsBody, selftestBody } from '../domain/schemas';
import { HttpError, parseJson, validate } from '../lib/http';
import { requireSignedAgent } from '../middleware/signature';
import { commandResult, heartbeat, reports } from '../services/agentApi';
import { DIAGNOSTICS_PREFIX, MAX_DIAGNOSTICS_BYTES, storeDiagnostics } from '../services/diagnostics';
import { enroll } from '../services/enroll';
import { manifestText } from '../services/releases';
import { storeSelftest } from '../services/selftest';

const CHANNELS = new Set(['canary', 'stable']);

export const agentRoutes = new Hono<AppEnv>();

agentRoutes.post('/enroll', async (c) => {
  const body = validate(enrollBody, parseJson(await c.req.text()));
  return c.json(await enroll(c.env, body, nowSeconds()));
});

// Diagnostics bundles are large: refuse an oversized or unsized upload before the signature
// check reads the body.
agentRoutes.use('/agent/diagnostics', async (c, next) => {
  const length = Number(c.req.header('Content-Length') ?? Number.NaN);
  if (!Number.isFinite(length)) throw new HttpError(411, 'length_required');
  if (length > MAX_DIAGNOSTICS_BYTES) throw new HttpError(413, 'diagnostics_too_large', { max_bytes: MAX_DIAGNOSTICS_BYTES });
  await next();
});

agentRoutes.use('*', requireSignedAgent);

agentRoutes.post('/heartbeat', async (c) => {
  const raw = c.get('rawBody');
  const hb = validate(heartbeatBody, parseJson(raw));
  return c.json(await heartbeat(c.env, c.get('agent'), hb, raw, nowSeconds()));
});

agentRoutes.post('/reports', async (c) => {
  const body = validate(reportsBody, parseJson(c.get('rawBody')));
  return c.json(await reports(c.env, c.get('agent'), body.reports, nowSeconds()));
});

/** `dl-agent selftest` uploads its run here (the hub keeps the last 20 per agent). */
agentRoutes.post('/agent/selftest', async (c) => {
  const raw = c.get('rawBody');
  if (new TextEncoder().encode(raw).byteLength > MAX_SELFTEST_BYTES) throw new HttpError(413, 'selftest_too_large');
  const body = validate(selftestBody, parseJson(raw));
  return c.json(await storeSelftest(c.env, c.get('agent'), body, nowSeconds()));
});

/** `collect_diagnostics` uploads its zip here (PROTOCOL.md §4.1); `?command_id=` links it to the command. */
agentRoutes.post('/agent/diagnostics', async (c) => {
  const commandId = c.req.query('command_id') ?? null;
  return c.json(await storeDiagnostics(c.env, c.get('agent'), c.get('rawBytes'), commandId, nowSeconds()));
});

agentRoutes.post('/commands/:id/result', async (c) => {
  const body = validate(commandResultBody, parseJson(c.get('rawBody')));
  return c.json(await commandResult(c.env, c.get('agent'), c.req.param('id'), body, nowSeconds()));
});

agentRoutes.get('/releases/:product/:channel', async (c) => {
  const channel = c.req.param('channel');
  if (!CHANNELS.has(channel)) throw new HttpError(404, 'no_release');
  const text = await manifestText(c.env, c.req.param('product'), channel as 'canary' | 'stable', c.get('agent'), nowSeconds());
  return c.body(text, 200, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' });
});

agentRoutes.get('/artifacts/:key{.+}', async (c) => {
  const key = c.req.param('key');
  // Diagnostics bundles belong to one tenant; only admin downloads them.
  if (key.startsWith(DIAGNOSTICS_PREFIX)) throw new HttpError(404, 'artifact_not_found');
  const object = await c.env.ARTIFACTS.get(key);
  if (!object) throw new HttpError(404, 'artifact_not_found');
  return c.body(object.body, 200, {
    'Content-Type': 'application/octet-stream',
    'Content-Length': String(object.size),
    'Cache-Control': 'private, no-store',
  });
});
