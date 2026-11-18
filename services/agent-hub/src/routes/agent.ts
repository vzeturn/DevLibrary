import { Hono } from 'hono';
import { nowSeconds, type AppEnv } from '../env';
import { commandResultBody, enrollBody, heartbeatBody, reportsBody } from '../domain/schemas';
import { HttpError, parseJson, validate } from '../lib/http';
import { requireSignedAgent } from '../middleware/signature';
import { commandResult, heartbeat, reports } from '../services/agentApi';
import { enroll } from '../services/enroll';
import { manifestText } from '../services/releases';

const CHANNELS = new Set(['canary', 'stable']);

export const agentRoutes = new Hono<AppEnv>();

agentRoutes.post('/enroll', async (c) => {
  const body = validate(enrollBody, parseJson(await c.req.text()));
  return c.json(await enroll(c.env, body, nowSeconds()));
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

agentRoutes.post('/commands/:id/result', async (c) => {
  const body = validate(commandResultBody, parseJson(c.get('rawBody')));
  return c.json(await commandResult(c.env, c.get('agent'), c.req.param('id'), body, nowSeconds()));
});

agentRoutes.get('/releases/:product/:channel', async (c) => {
  const channel = c.req.param('channel');
  if (!CHANNELS.has(channel)) throw new HttpError(404, 'no_release');
  const text = await manifestText(c.env, c.req.param('product'), channel as 'canary' | 'stable');
  return c.body(text, 200, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' });
});

agentRoutes.get('/artifacts/:key{.+}', async (c) => {
  const object = await c.env.ARTIFACTS.get(c.req.param('key'));
  if (!object) throw new HttpError(404, 'artifact_not_found');
  return c.body(object.body, 200, {
    'Content-Type': 'application/octet-stream',
    'Content-Length': String(object.size),
    'Cache-Control': 'private, no-store',
  });
});
