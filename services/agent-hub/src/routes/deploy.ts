import { Hono } from 'hono';
import { z } from 'zod';
import { nowSeconds, type AppEnv } from '../env';
import { parseJson, validate } from '../lib/http';
import { renderDeployPage } from '../install/deployPage';
import { deployStatus } from '../services/deployStatus';

const statusBody = z.object({ code: z.string().min(4).max(64) });

/**
 * Public guided install (RT-0292): `GET /deploy#code=…` is one page with three steps and a live
 * status; `POST /deploy/status {code}` answers that code's progress. The code is the capability:
 * it never sits in a URL the hub sees (fragment, then a POST body), and the status only describes
 * the machine that used it.
 */
export const deployRoutes = new Hono<AppEnv>();

deployRoutes.get('/', (c) => {
  const nonce = crypto.randomUUID().replace(/-/g, '');
  return c.html(renderDeployPage(new URL(c.req.url).origin, nonce), 200, {
    'Cache-Control': 'no-store',
    'Content-Security-Policy': `default-src 'none'; script-src 'nonce-${nonce}'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
    'Referrer-Policy': 'no-referrer',
    'X-Content-Type-Options': 'nosniff',
  });
});

deployRoutes.post('/status', async (c) => {
  const body = validate(statusBody, parseJson(await c.req.text()));
  return c.json(await deployStatus(c.env, body.code, nowSeconds()), 200, { 'Cache-Control': 'no-store' });
});
