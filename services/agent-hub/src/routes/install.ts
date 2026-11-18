import { Hono } from 'hono';
import { z } from 'zod';
import { nowSeconds, type AppEnv, type Channel } from '../env';
import { installScript } from '../install/script';
import { HttpError, validate } from '../lib/http';
import { download, infoText, installInfo } from '../services/install';

/** Public: a machine being installed has no identity yet. Nothing here is cached (a stale copy broke PDAOrder installs). */
const NO_CACHE = { 'Cache-Control': 'no-cache, no-store' };

const name = z.string().regex(/^[A-Za-z0-9._-]{1,64}$/);
const params = z.object({
  product: name,
  channel: z.enum(['canary', 'stable']),
  target: z.string().regex(/^[a-z0-9]+-[a-z0-9_]+$/),
});

export const installRoutes = new Hono<AppEnv>();

installRoutes.get('/info/:product/:channel/:target', async (c) => {
  const p = validate(params, c.req.param());
  const info = await installInfo(c.env, p.product, p.channel as Channel, p.target, nowSeconds());
  return c.body(infoText(info), 200, { 'Content-Type': 'text/plain; charset=utf-8', ...NO_CACHE });
});

installRoutes.get('/download/:product/:channel/:version/:target', async (c) => {
  const p = validate(params, c.req.param());
  const version = validate(name, c.req.param('version'));
  const d = await download(c.env, new URL(c.req.url).origin, p.product, p.channel as Channel, version, p.target);
  if (d.kind === 'redirect') return c.redirect(d.url, 302);
  return c.body(d.object.body, 200, {
    'Content-Type': 'application/octet-stream',
    'Content-Length': String(d.object.size),
    'Content-Disposition': `attachment; filename="${p.product}.exe"`,
    ...NO_CACHE,
  });
});

/** Served at the root as `/install.ps1`. */
export function installScriptResponse(url: string): Response {
  const origin = new URL(url).origin;
  if (!origin.startsWith('http')) throw new HttpError(400, 'bad_origin');
  return new Response(installScript(origin), {
    headers: { 'Content-Type': 'text/plain; charset=utf-8', ...NO_CACHE },
  });
}
