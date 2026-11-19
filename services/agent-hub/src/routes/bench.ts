import { Hono } from 'hono';
import { nowSeconds, type AppEnv } from '../env';
import { fetchBenchScript } from '../services/bench';

const HEADERS = { 'Content-Type': 'text/plain; charset=utf-8', 'Cache-Control': 'no-store' };

/** Public: the token in the path is the capability. One answer for every miss, so nothing leaks about which. */
export const benchRoutes = new Hono<AppEnv>();

benchRoutes.get('/:token', async (c) => {
  const script = await fetchBenchScript(c.env, c.req.param('token'), new URL(c.req.url).origin, nowSeconds());
  if (script === null) {
    return c.body('Write-Host "[bench] This link is unknown, expired, revoked or used up: ask the hub admin for a new one." -ForegroundColor Red\n', 404, HEADERS);
  }
  return c.body(script, 200, HEADERS);
});
