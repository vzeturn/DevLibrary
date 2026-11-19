import { Hono } from 'hono';
import { z } from 'zod';
import { nowSeconds, type AppEnv, type Channel } from '../env';
import { installScript, legacyScript } from '../install/script';
import { uninstallScript } from '../install/uninstallScript';
import { hubUrls } from '../domain/hubUrls';
import { HttpError, validate } from '../lib/http';
import { download, infoText, installInfo, releaseManifest } from '../services/install';

/** Public: a machine being installed has no identity yet. Nothing here is cached (a stale copy broke PDAOrder installs). */
const NO_CACHE = { 'Cache-Control': 'no-cache, no-store' };

const name = z.string().regex(/^[A-Za-z0-9._-]{1,64}$/);
const params = z.object({
  product: name,
  channel: z.enum(['canary', 'stable']),
  target: z.string().regex(/^[a-z0-9]+-[a-z0-9_]+$/),
});

export const installRoutes = new Hono<AppEnv>();

const version = z.string().regex(/^\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$/);

installRoutes.get('/info/:product/:channel/:target', async (c) => {
  const p = validate(params, c.req.param());
  const pinned = c.req.query('version');
  const v = pinned === undefined ? undefined : validate(version, pinned);
  const info = await installInfo(c.env, p.product, p.channel as Channel, p.target, nowSeconds(), v);
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

/** The signed manifest of one release, for the installer's `verify-self` check. */
installRoutes.get('/manifest/:product/:channel/:version', async (c) => {
  const p = validate(params.omit({ target: true }), c.req.param());
  const v = validate(version, c.req.param('version'));
  const text = await releaseManifest(c.env, p.product, p.channel as Channel, v);
  return c.body(text, 200, { 'Content-Type': 'application/json; charset=utf-8', ...NO_CACHE });
});

/** The previous installer: `/install.ps1` runs it for a build without `dl-agent install`. */
installRoutes.get('/legacy.ps1', (c) => scriptResponse(c.req.url, (origin) => legacyScript(origin, hubUrls(c.env.HUB_URLS))));

function scriptResponse(url: string, render: (origin: string) => string): Response {
  const origin = new URL(url).origin;
  if (!origin.startsWith('http')) throw new HttpError(400, 'bad_origin');
  return new Response(render(origin), {
    headers: { 'Content-Type': 'text/plain; charset=utf-8', ...NO_CACHE },
  });
}

/** Served at the root as `/uninstall.ps1`. */
export function uninstallScriptResponse(url: string): Response {
  return scriptResponse(url, uninstallScript);
}

/** Served at the root as `/install.ps1`. */
export function installScriptResponse(url: string, hubs: readonly string[] = []): Response {
  return scriptResponse(url, (origin) => installScript(origin, hubs));
}
