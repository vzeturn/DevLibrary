import { Hono } from 'hono';
import type { AppEnv } from '../env';
import { constantTimeEqual } from '../lib/crypto';
import { logError } from '../lib/http';
import { callTool, toolList, ToolError, type AdminGet } from '../services/mcpTools';

/** Protocol revision answered when the client names none. */
export const MCP_PROTOCOL = '2025-06-18';

interface RpcRequest {
  jsonrpc?: unknown;
  id?: unknown;
  method?: unknown;
  params?: unknown;
}

const rpcError = (id: unknown, code: number, message: string) => ({ jsonrpc: '2.0', id: id ?? null, error: { code, message } });
const rpcResult = (id: unknown, result: unknown) => ({ jsonrpc: '2.0', id, result });

/**
 * Read-only ops console for the Request Tracking hub (RT-0300): MCP over HTTP POST (JSON-RPC,
 * JSON answers), `Authorization: Bearer MCP_TOKEN`. It reads the admin API through `adminGet`
 * (GET only, the admin token never leaves the Worker), the public repository and the published
 * test runs. A missing MCP_TOKEN locks it.
 */
export function mcpRoutes(adminGetFor: (origin: string, env: AppEnv['Bindings']) => AdminGet): Hono<AppEnv> {
  const routes = new Hono<AppEnv>();

  routes.use('*', async (c, next) => {
    const expected = c.env.MCP_TOKEN;
    const header = c.req.header('Authorization') ?? '';
    const token = header.startsWith('Bearer ') ? header.slice(7).trim() : '';
    if (!expected || !token || !(await constantTimeEqual(token, expected))) {
      c.header('WWW-Authenticate', 'Bearer');
      return c.json({ error: 'unauthorized' }, 401);
    }
    await next();
  });

  routes.get('/', (c) => c.json({ error: 'method_not_allowed' }, 405, { Allow: 'POST' }));

  routes.post('/', async (c) => {
    let req: RpcRequest;
    try {
      req = (await c.req.json()) as RpcRequest;
    } catch (err) {
      logError('mcp_parse_error', err);
      return c.json(rpcError(null, -32700, 'parse error'), 400);
    }
    if (!req || typeof req !== 'object' || Array.isArray(req) || req.jsonrpc !== '2.0' || typeof req.method !== 'string') {
      return c.json(rpcError(null, -32600, 'invalid request'), 400);
    }
    // A notification (no id) gets no answer.
    if (req.id === undefined) return c.body(null, 202);
    const params = (req.params && typeof req.params === 'object' ? req.params : {}) as Record<string, unknown>;

    switch (req.method) {
      case 'initialize':
        return c.json(
          rpcResult(req.id, {
            protocolVersion: typeof params.protocolVersion === 'string' ? params.protocolVersion : MCP_PROTOCOL,
            capabilities: { tools: { listChanged: false } },
            serverInfo: { name: 'devlibrary-agent-hub-ops', version: '1.0.0' },
            instructions: 'Read-only console of the DevLibrary agent hub (dl-agent). Every tool only reads.',
          }),
        );
      case 'ping':
        return c.json(rpcResult(req.id, {}));
      case 'tools/list':
        return c.json(rpcResult(req.id, { tools: toolList() }));
      case 'tools/call': {
        const name = typeof params.name === 'string' ? params.name : '';
        const args = (params.arguments && typeof params.arguments === 'object' ? params.arguments : {}) as Record<string, unknown>;
        const ctx = { env: c.env, adminGet: adminGetFor(new URL(c.req.url).origin, c.env), fetch: (input: RequestInfo | URL, init?: RequestInit) => fetch(input, init) };
        try {
          const out = await callTool(ctx, name, args);
          return c.json(rpcResult(req.id, { content: [{ type: 'text', text: JSON.stringify(out, null, 1) }] }));
        } catch (err) {
          if (!(err instanceof ToolError)) logError('mcp_tool_failed', err, { tool: name });
          const message = err instanceof Error ? err.message : String(err);
          return c.json(rpcResult(req.id, { content: [{ type: 'text', text: message }], isError: true }));
        }
      }
      default:
        return c.json(rpcError(req.id, -32601, `method not found: ${req.method}`));
    }
  });

  return routes;
}
