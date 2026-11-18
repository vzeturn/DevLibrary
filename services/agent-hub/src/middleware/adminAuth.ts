import type { MiddlewareHandler } from 'hono';
import type { AppEnv } from '../env';
import { constantTimeEqual } from '../lib/crypto';
import { HttpError } from '../lib/http';

/** Bearer ADMIN_TOKEN, compared in constant time. A missing secret locks admin out instead of opening it. */
export const requireAdmin: MiddlewareHandler<AppEnv> = async (c, next) => {
  const expected = c.env.ADMIN_TOKEN;
  const header = c.req.header('Authorization') ?? '';
  const token = header.startsWith('Bearer ') ? header.slice(7).trim() : '';
  if (!expected || !token || !(await constantTimeEqual(token, expected))) {
    c.header('WWW-Authenticate', 'Bearer');
    throw new HttpError(401, 'unauthorized');
  }
  await next();
};
