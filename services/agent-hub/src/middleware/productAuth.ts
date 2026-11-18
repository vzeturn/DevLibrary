import type { MiddlewareHandler } from 'hono';
import { nowSeconds, type AppEnv } from '../env';
import { HttpError } from '../lib/http';
import { authenticateProductKey } from '../services/productKeys';

/** Bearer product key, live and issued for the `:product` in the path. */
export const requireProductKey: MiddlewareHandler<AppEnv> = async (c, next) => {
  const header = c.req.header('Authorization') ?? '';
  const token = header.startsWith('Bearer ') ? header.slice(7).trim() : '';
  const row = token ? await authenticateProductKey(c.env, token, nowSeconds()) : null;
  if (!row || row.product !== c.req.param('product')) {
    c.header('WWW-Authenticate', 'Bearer');
    throw new HttpError(401, 'unauthorized');
  }
  await next();
};
