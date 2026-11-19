import { Hono } from 'hono';
import { z } from 'zod';
import { nowSeconds, type AppEnv } from '../env';
import { benchBody } from '../domain/bench';
import { demoBenchBody } from '../domain/demo';
import { productName, severity } from '../domain/schemas';
import { HttpError, parseJson, validate } from '../lib/http';
import { installCommand } from '../install/script';
import { auditAdmin } from '../middleware/adminAudit';
import { requireAdmin } from '../middleware/adminAuth';
import { listAudit } from '../repo/audit';
import { listDiagnostics } from '../repo/diagnostics';
import { listCodes, revokeCode } from '../repo/enrollCodes';
import { listProductKeys, revokeProductKey } from '../repo/productKeys';
import { listReleases, listPromotions } from '../repo/releases';
import { listReports } from '../repo/reports';
import { listTenants, upsertTenant } from '../repo/tenants';
import { agentsView, commandsView, createEnrollCode, enqueueCommand, revoke, setChannel } from '../services/admin';
import { setConfig } from '../services/config';
import { benchView, createBench, revokeBenchBundle } from '../services/bench';
import { createDemoBench } from '../services/demoBench';
import { diagnosticsObject } from '../services/diagnostics';
import { createProductKey } from '../services/productKeys';
import { removeRequirement, requirementOf, setRequirement, siteReadiness, surveyOf } from '../services/sites';
import { allowRelease, disallowRelease } from '../services/lanes';
import { holdsView, holdTenant, pauseRelease, releaseHold, resumeRelease } from '../services/holds';
import { gateStatus, promote, publishCanary } from '../services/releases';
import { selftestView } from '../services/selftest';
import { renderStatusPage } from './statusPage';

const id = z.string().regex(/^[A-Za-z0-9._-]{1,64}$/, 'letters, digits, . _ - only');

const tenantBody = z.object({ name: z.string().min(1).max(200), demo: z.boolean().default(false) });
const codeBody = z.object({
  tenant_id: id,
  site_id: id,
  ttl_seconds: z.number().int().min(60).max(30 * 86400).default(86400),
  config: z.record(z.string(), z.unknown()).optional(),
});
const productKeyBody = z.object({ product: productName, label: z.string().min(1).max(200) });
const channelBody = z.object({ channel: z.enum(['canary', 'stable']).nullable() });
const commandBody = z.object({
  agent_id: z.string().min(1),
  kind: z.string().regex(/^[a-z][a-z0-9_.]{0,63}$/),
  module: z.string().min(1).max(100).optional(),
  args: z.record(z.string(), z.unknown()).default({}),
  ttl_seconds: z.number().int().min(10).max(7 * 86400).default(3600),
});
const configBody = z.object({ body: z.record(z.string(), z.unknown()) });
const promoteBody = z.object({ manifest: z.string().min(2), force: z.boolean().optional(), reason: z.string().max(2000).optional() });
/** A tenant (every site) or one site of it, opted in to a major release. */
const allowBody = z.object({ tenant_id: id, site_id: id.optional() });
const pauseBody = z.object({ reason: z.string().max(2000).optional() });
const holdBody = z.object({ site_id: id.optional(), product: productName.optional(), reason: z.string().max(2000).optional() });
const reportQuery = z.object({
  tenant_id: z.string().optional(),
  severity: severity.optional(),
  fingerprint: z.string().optional(),
  version: z.string().optional(),
  limit: z.coerce.number().int().min(1).max(1000).default(200),
});
const diagnosticsQuery = z.object({
  agent_id: z.string().optional(),
  tenant_id: z.string().optional(),
  limit: z.coerce.number().int().min(1).max(500).default(50),
});
const auditQuery = z.object({
  since: z.coerce.number().int().min(0).optional(),
  actor: z.string().max(320).optional(),
  path_prefix: z.string().max(200).optional(),
  limit: z.coerce.number().int().min(1).max(1000).default(200),
});

async function jsonBody(req: { text(): Promise<string> }): Promise<unknown> {
  return parseJson(await req.text());
}

/** Like jsonBody, but an empty body means `{}` (every field optional). */
async function optionalBody(req: { text(): Promise<string> }): Promise<unknown> {
  const text = await req.text();
  return text.trim() ? parseJson(text) : {};
}

export const adminRoutes = new Hono<AppEnv>();
adminRoutes.use('*', requireAdmin);
adminRoutes.use('*', auditAdmin);

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
  const created = await createEnrollCode(c.env, body, nowSeconds());
  return c.json({ ...created, install_command: installCommand(new URL(c.req.url).origin, created.code) }, 201);
});
adminRoutes.get('/enroll-codes', async (c) => c.json({ codes: await listCodes(c.env.DB, c.req.query('tenant_id')) }));
adminRoutes.delete('/enroll-codes/:id', async (c) => {
  if (!(await revokeCode(c.env.DB, c.req.param('id'), nowSeconds()))) throw new HttpError(404, 'code_not_revocable');
  return c.json({});
});

adminRoutes.post('/product-keys', async (c) => {
  const body = validate(productKeyBody, await jsonBody(c.req));
  return c.json(await createProductKey(c.env, body.product, body.label, nowSeconds()), 201);
});
adminRoutes.get('/product-keys', async (c) => c.json({ keys: await listProductKeys(c.env.DB) }));
adminRoutes.delete('/product-keys/:id', async (c) => {
  if (!(await revokeProductKey(c.env.DB, c.req.param('id'), nowSeconds()))) throw new HttpError(404, 'product_key_not_found');
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
adminRoutes.get('/agents/:id/survey', async (c) => c.json(await surveyOf(c.env, c.req.param('id'))));
adminRoutes.get('/agents/:id/selftest', async (c) => c.json(await selftestView(c.env, c.req.param('id'))));
adminRoutes.put('/agents/:id/config', async (c) => {
  const body = validate(configBody, await jsonBody(c.req));
  return c.json(await setConfig(c.env.DB, c.req.param('id'), body.body, nowSeconds()));
});

adminRoutes.post('/commands', async (c) => {
  const body = validate(commandBody, await jsonBody(c.req));
  return c.json(await enqueueCommand(c.env, body, nowSeconds()), 201);
});
adminRoutes.get('/commands', async (c) => c.json({ commands: await commandsView(c.env, c.req.query('agent_id'), nowSeconds()) }));

/** Bench bundles: one `irm … | iex` line that sets up a test PC (README "Bench (test PC)"). */
adminRoutes.post('/bench', async (c) => {
  const body = validate(benchBody, await jsonBody(c.req));
  return c.json(await createBench(c.env, body, new URL(c.req.url).origin, nowSeconds()), 201);
});
adminRoutes.get('/bench', async (c) => c.json({ bundles: await benchView(c.env, nowSeconds()) }));
adminRoutes.delete('/bench/:id', async (c) => {
  await revokeBenchBundle(c.env, c.req.param('id'), nowSeconds());
  return c.json({});
});

/**
 * Demo bench (RT-0261): a one-time canary enrolment code for a demo tenant with every module
 * enabled, the `install.ps1 -Demo` line and the variables the operator sets for the module codes.
 */
adminRoutes.post('/demo-bench', async (c) => {
  const body = validate(demoBenchBody, await jsonBody(c.req));
  const set = await createDemoBench(c.env, body, new URL(c.req.url).origin, nowSeconds());
  return c.json(set, 201, { 'Cache-Control': 'no-store' });
});

adminRoutes.get('/releases', async (c) => c.json({ releases: await listReleases(c.env.DB) }));
adminRoutes.put('/releases/:product/:version', async (c) => {
  const m = await publishCanary(c.env, c.req.param('product'), c.req.param('version'), await c.req.text(), nowSeconds());
  return c.json({ product: m.product, version: m.version, channel: m.channel, published_at: m.published_at });
});
adminRoutes.get('/releases/:product/:version/gate', async (c) => {
  const { product, version } = c.req.param();
  const gate = await gateStatus(c.env, product, version, nowSeconds());
  return c.json({ ...gate, promotions: await listPromotions(c.env.DB, product, version) });
});
adminRoutes.post('/releases/:product/:version/promote', async (c) => {
  const body = validate(promoteBody, await jsonBody(c.req));
  const { product, version } = c.req.param();
  return c.json(await promote(c.env, product, version, body, nowSeconds()));
});
/** Majors are never offered on their own (PROTOCOL §5.2): opt a tenant or a site in, or send update_now. */
adminRoutes.post('/releases/:product/:version/allow', async (c) => {
  const body = validate(allowBody, await jsonBody(c.req));
  const { product, version } = c.req.param();
  return c.json({ product, version, ...(await allowRelease(c.env, product, version, body, nowSeconds())) }, 201);
});
adminRoutes.delete('/releases/:product/:version/allow', async (c) => {
  const body = validate(allowBody, await jsonBody(c.req));
  const { product, version } = c.req.param();
  await disallowRelease(c.env, product, version, body);
  return c.json({});
});

/** Kill switch (PROTOCOL §5.3): a paused release is never offered, on canary or stable. */
adminRoutes.post('/releases/:product/:version/pause', async (c) => {
  const body = validate(pauseBody, await optionalBody(c.req));
  const { product, version } = c.req.param();
  return c.json(await pauseRelease(c.env, product, version, body.reason, nowSeconds()), 201);
});
adminRoutes.post('/releases/:product/:version/resume', async (c) => {
  const { product, version } = c.req.param();
  await resumeRelease(c.env, product, version);
  return c.json({});
});

/** Hold a tenant, or one of its sites, off updates; update_now remains the operator's override. */
adminRoutes.get('/tenants/:tenant/hold', async (c) => c.json({ holds: await holdsView(c.env, validate(id, c.req.param('tenant'))) }));
adminRoutes.post('/tenants/:tenant/hold', async (c) => {
  const body = validate(holdBody, await optionalBody(c.req));
  return c.json(await holdTenant(c.env, validate(id, c.req.param('tenant')), body, nowSeconds()), 201);
});
adminRoutes.delete('/tenants/:tenant/hold', async (c) => {
  const body = validate(holdBody, await optionalBody(c.req));
  await releaseHold(c.env, validate(id, c.req.param('tenant')), body);
  return c.json({});
});

adminRoutes.get('/products/:product/site-requirements', async (c) => {
  const product = validate(id, c.req.param('product'));
  const req = await requirementOf(c.env, product);
  if (!req) throw new HttpError(404, 'no_site_requirement');
  return c.json(req);
});
adminRoutes.put('/products/:product/site-requirements', async (c) => {
  const product = validate(id, c.req.param('product'));
  return c.json(await setRequirement(c.env, product, await jsonBody(c.req), nowSeconds()));
});
adminRoutes.delete('/products/:product/site-requirements', async (c) => {
  await removeRequirement(c.env, validate(id, c.req.param('product')));
  return c.json({});
});
/** Which sites are ready for a product, before moving them onto it. */
adminRoutes.get('/products/:product/sites', async (c) => {
  const product = validate(id, c.req.param('product'));
  const req = await requirementOf(c.env, product);
  if (!req) throw new HttpError(404, 'no_site_requirement');
  const tenantId = c.req.query('tenant_id');
  const sites = await siteReadiness(c.env, req, tenantId ? { tenantId } : {}, nowSeconds());
  return c.json({ product, requirement: req, ready: sites.filter((s) => s.eligible).length, sites });
});

adminRoutes.get('/reports', async (c) => {
  const q = validate(reportQuery, c.req.query());
  const rows = await listReports(c.env.DB, q);
  return c.json({ reports: rows.map((r) => ({ ...r, context: JSON.parse(r.context) as unknown })) });
});

/** Uploaded `collect_diagnostics` bundles, newest first. */
adminRoutes.get('/diagnostics', async (c) => {
  const q = validate(diagnosticsQuery, c.req.query());
  return c.json({ diagnostics: await listDiagnostics(c.env.DB, q) });
});

adminRoutes.get('/diagnostics/:id/download', async (c) => {
  const { name, object } = await diagnosticsObject(c.env, c.req.param('id'));
  return c.body(object.body, 200, {
    'Content-Type': 'application/zip',
    'Content-Length': String(object.size),
    'Content-Disposition': `attachment; filename="${name}"`,
    'Cache-Control': 'private, no-store',
  });
});

/** Every admin change (method, path, scrubbed body, who, outcome), newest first. */
adminRoutes.get('/audit', async (c) => {
  const q = validate(auditQuery, c.req.query());
  return c.json({ audit: await listAudit(c.env.DB, q) });
});
