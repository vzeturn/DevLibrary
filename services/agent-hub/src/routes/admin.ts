import { Hono } from 'hono';
import { z } from 'zod';
import { nowSeconds, type AppEnv } from '../env';
import { severity } from '../domain/schemas';
import { HttpError, parseJson, validate } from '../lib/http';
import { requireAdmin } from '../middleware/adminAuth';
import { listCodes, revokeCode } from '../repo/enrollCodes';
import { listReleases, listPromotions } from '../repo/releases';
import { listReports } from '../repo/reports';
import { listTenants, upsertTenant } from '../repo/tenants';
import { agentsView, commandsView, createEnrollCode, enqueueCommand, revoke, setChannel } from '../services/admin';
import { setConfig } from '../services/config';
import { gateStatus, promote, publishCanary } from '../services/releases';
import { renderStatusPage } from './statusPage';

const id = z.string().regex(/^[A-Za-z0-9._-]{1,64}$/, 'letters, digits, . _ - only');

const tenantBody = z.object({ name: z.string().min(1).max(200), demo: z.boolean().default(false) });
const codeBody = z.object({ tenant_id: id, site_id: id, ttl_seconds: z.number().int().min(60).max(30 * 86400).default(86400) });
const channelBody = z.object({ channel: z.enum(['canary', 'stable']).nullable() });
const commandBody = z.object({
  agent_id: z.string().min(1),
  kind: z.string().regex(/^[a-z][a-z0-9_]{0,63}$/),
  module: z.string().min(1).max(100).optional(),
  args: z.record(z.string(), z.unknown()).default({}),
  ttl_seconds: z.number().int().min(10).max(7 * 86400).default(3600),
});
const configBody = z.object({ body: z.record(z.string(), z.unknown()) });
const promoteBody = z.object({ manifest: z.string().min(2), force: z.boolean().optional(), reason: z.string().max(2000).optional() });
const reportQuery = z.object({
  tenant_id: z.string().optional(),
  severity: severity.optional(),
  fingerprint: z.string().optional(),
  version: z.string().optional(),
  limit: z.coerce.number().int().min(1).max(1000).default(200),
});

async function jsonBody(req: { text(): Promise<string> }): Promise<unknown> {
  return parseJson(await req.text());
}

export const adminRoutes = new Hono<AppEnv>();
adminRoutes.use('*', requireAdmin);

adminRoutes.get('/', async (c) => c.html(await renderStatusPage(c.env, nowSeconds())));

adminRoutes.get('/tenants', async (c) => c.json({ tenants: await listTenants(c.env.DB) }));
adminRoutes.put('/tenants/:id', async (c) => {
  const tenantId = validate(id, c.req.param('id'));
  const body = validate(tenantBody, await jsonBody(c.req));
  await upsertTenant(c.env.DB, { id: tenantId, ...body }, nowSeconds());
  return c.json({ id: tenantId, ...body });
});

adminRoutes.post('/enroll-codes', async (c) => {
  const body = validate(codeBody, await jsonBody(c.req));
  return c.json(await createEnrollCode(c.env, body, nowSeconds()), 201);
});
adminRoutes.get('/enroll-codes', async (c) => c.json({ codes: await listCodes(c.env.DB, c.req.query('tenant_id')) }));
adminRoutes.delete('/enroll-codes/:id', async (c) => {
  if (!(await revokeCode(c.env.DB, c.req.param('id'), nowSeconds()))) throw new HttpError(404, 'code_not_revocable');
  return c.json({});
});

adminRoutes.get('/agents', async (c) => c.json({ agents: await agentsView(c.env, c.req.query('tenant_id'), nowSeconds()) }));
adminRoutes.put('/agents/:id/channel', async (c) => {
  const body = validate(channelBody, await jsonBody(c.req));
  await setChannel(c.env, c.req.param('id'), body.channel);
  return c.json({ channel_override: body.channel });
});
adminRoutes.post('/agents/:id/revoke', async (c) => {
  await revoke(c.env, c.req.param('id'), nowSeconds());
  return c.json({});
});
adminRoutes.put('/agents/:id/config', async (c) => {
  const body = validate(configBody, await jsonBody(c.req));
  return c.json(await setConfig(c.env.DB, c.req.param('id'), body.body, nowSeconds()));
});

adminRoutes.post('/commands', async (c) => {
  const body = validate(commandBody, await jsonBody(c.req));
  return c.json(await enqueueCommand(c.env, body, nowSeconds()), 201);
});
adminRoutes.get('/commands', async (c) => c.json({ commands: await commandsView(c.env, c.req.query('agent_id'), nowSeconds()) }));

adminRoutes.get('/releases', async (c) => c.json({ releases: await listReleases(c.env.DB) }));
adminRoutes.put('/releases/:product/:version', async (c) => {
  const m = await publishCanary(c.env, c.req.param('product'), c.req.param('version'), await c.req.text(), nowSeconds());
  return c.json({ product: m.product, version: m.version, channel: m.channel, published_at: m.published_at });
});
adminRoutes.get('/releases/:product/:version/gate', async (c) => {
  const { product, version } = c.req.param();
  const gate = await gateStatus(c.env, product, version);
  return c.json({ ...gate, promotions: await listPromotions(c.env.DB, product, version) });
});
adminRoutes.post('/releases/:product/:version/promote', async (c) => {
  const body = validate(promoteBody, await jsonBody(c.req));
  const { product, version } = c.req.param();
  return c.json(await promote(c.env, product, version, body, nowSeconds()));
});

adminRoutes.get('/reports', async (c) => {
  const q = validate(reportQuery, c.req.query());
  const rows = await listReports(c.env.DB, q);
  return c.json({ reports: rows.map((r) => ({ ...r, context: JSON.parse(r.context) as unknown })) });
});
