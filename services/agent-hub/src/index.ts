import { Hono } from 'hono';
import { nowSeconds, type AppEnv, type Env } from './env';
import { HttpError, logError } from './lib/http';
import { NONCE_TTL_SECONDS } from './middleware/signature';
import { pruneNonces } from './repo/agents';
import { adminRoutes } from './routes/admin';
import { agentRoutes } from './routes/agent';
import { benchRoutes } from './routes/bench';
import { installRoutes, installScriptResponse } from './routes/install';
import { productRoutes } from './routes/products';

export const app = new Hono<AppEnv>();

app.get('/healthz', (c) => c.json({ ok: true }));
app.get('/install.ps1', (c) => installScriptResponse(c.req.url));
app.route('/install', installRoutes);
app.route('/products', productRoutes);
app.route('/v1', agentRoutes);
app.route('/admin', adminRoutes);
app.route('/bench', benchRoutes);

app.notFound((c) => c.json({ error: 'not_found' }, 404));
app.onError((err, c) => {
  if (err instanceof HttpError) return c.json({ error: err.code, ...err.extra }, err.status);
  logError('unhandled', err, { method: c.req.method, path: new URL(c.req.url).pathname });
  return c.json({ error: 'internal' }, 500);
});

export default {
  fetch: app.fetch,
  async scheduled(_event: ScheduledController, env: Env, ctx: ExecutionContext): Promise<void> {
    ctx.waitUntil(
      pruneNonces(env.DB, nowSeconds() - NONCE_TTL_SECONDS).catch((err: unknown) => {
        logError('prune_nonces_failed', err);
        throw err;
      }),
    );
  },
} satisfies ExportedHandler<Env>;
