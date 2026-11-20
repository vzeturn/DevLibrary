import { Hono } from 'hono';
import { z } from 'zod';
import { nowSeconds, type AppEnv } from '../env';
import { productName } from '../domain/schemas';
import { parseJson, validate } from '../lib/http';
import { installCommand } from '../install/script';
import { deployUrl } from '../install/deployPage';
import { requireProductKey } from '../middleware/productAuth';
import { mintProductCode } from '../services/productKeys';

const id = z.string().regex(/^[A-Za-z0-9._-]{1,64}$/, 'letters, digits, . _ - only');
const codeBody = z.object({
  external_id: id,
  name: z.string().min(1).max(200),
  site_id: id.default('main'),
  ttl_seconds: z.number().int().min(60).max(7 * 86400).default(86400),
  demo: z.boolean().default(false),
});

/** What a product platform (holding a product key) may do on the hub: mint codes for its tenants. */
export const productRoutes = new Hono<AppEnv>();

productRoutes.use('/:product/*', requireProductKey);

productRoutes.post('/:product/enroll-codes', async (c) => {
  const product = validate(productName, c.req.param('product'));
  const body = validate(codeBody, parseJson(await c.req.text()));
  const created = await mintProductCode(c.env, product, body, nowSeconds());
  const origin = new URL(c.req.url).origin;
  return c.json({ ...created, install_command: installCommand(origin, created.code), deploy_url: deployUrl(origin, created.code) }, 201);
});
