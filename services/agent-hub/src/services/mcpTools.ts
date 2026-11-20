import type { Env } from '../env';
import { testRunsView } from './testRuns';

/** The repository the console reads (public on GitHub; no token involved). */
export const REPO = 'vzeturn/DevLibrary';
/** Largest file or admin answer handed back to the caller, in characters. */
export const MAX_TEXT = 200_000;

/** Calls a GET route of the hub's own admin API (the token stays inside the Worker). */
export type AdminGet = (path: string) => Promise<Response>;

export interface ToolContext {
  env: Env;
  adminGet: AdminGet;
  fetch: typeof fetch;
}

interface ToolDef {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
  run: (ctx: ToolContext, args: Record<string, unknown>) => Promise<unknown>;
}

export class ToolError extends Error {}

const str = (description: string) => ({ type: 'string', description });
const obj = (properties: Record<string, unknown>, required: string[] = []) => ({ type: 'object', properties, required, additionalProperties: false });

function arg(args: Record<string, unknown>, name: string, pattern?: RegExp): string {
  const v = args[name];
  if (typeof v !== 'string' || v.length === 0 || v.length > 500 || (pattern && !pattern.test(v))) throw new ToolError(`invalid ${name}`);
  return v;
}

function optArg(args: Record<string, unknown>, name: string, pattern?: RegExp): string | undefined {
  return args[name] === undefined || args[name] === null || args[name] === '' ? undefined : arg(args, name, pattern);
}

const ID = /^[A-Za-z0-9._-]{1,64}$/;
const REF = /^[A-Za-z0-9._/-]{1,100}$/;
const REPO_PATH = /^(?!.*\.\.)[A-Za-z0-9._/@ -]{0,300}$/;
/** Admin GET paths the console may not call: bundles may hold customer data, the HTML page is for people. */
const BLOCKED_ADMIN = [/^\/admin\/diagnostics\/[^/]+\/download/, /^\/admin\/?(\?|$)/];

function qs(params: Record<string, string | undefined>): string {
  const q = new URLSearchParams(Object.entries(params).filter((e): e is [string, string] => e[1] !== undefined));
  const s = q.toString();
  return s ? `?${s}` : '';
}

async function adminJson(ctx: ToolContext, path: string): Promise<unknown> {
  const traversal = path.includes('..') || path.includes('//') || path.includes('%2');
  if (!path.startsWith('/admin/') || traversal || BLOCKED_ADMIN.some((re) => re.test(path))) throw new ToolError(`path not allowed: ${path}`);
  const res = await ctx.adminGet(path);
  const text = await res.text();
  const body = text.length > MAX_TEXT ? `${text.slice(0, MAX_TEXT)}…[truncated]` : text;
  const json = (res.headers.get('Content-Type') ?? '').includes('json') && text.length <= MAX_TEXT;
  return { status: res.status, body: json ? (JSON.parse(text) as unknown) : body };
}

async function github(ctx: ToolContext, url: string): Promise<Response> {
  return ctx.fetch(url, { headers: { 'User-Agent': 'devlibrary-agent-hub-ops', Accept: 'application/vnd.github+json' } });
}

const TOOLS: ToolDef[] = [
  {
    name: 'hub_test_results',
    description: 'Latest published `pnpm test` runs of the hub (services/agent-hub): commit, counts, typecheck, failures. Newest first.',
    inputSchema: obj({ suite: str('Suite name (default agent-hub).'), limit: { type: 'integer', minimum: 1, maximum: 20, description: 'Runs to return (default 1).' } }),
    run: async (ctx, a) => {
      const suite = optArg(a, 'suite', /^[a-z][a-z0-9-]{1,40}$/) ?? 'agent-hub';
      const limit = typeof a.limit === 'number' ? a.limit : 1;
      const runs = await testRunsView(ctx.env, suite, limit);
      return runs.length ? { runs } : { runs, note: 'no run published yet: a session runs `pnpm test:publish` in services/agent-hub' };
    },
  },
  {
    name: 'repo_file',
    description: `Reads one file of the ${REPO} repository at a ref (default main), as text.`,
    inputSchema: obj({ path: str('Path from the repository root, e.g. crates/dl-agent/README.md.'), ref: str('Branch, tag or commit (default main).') }, ['path']),
    run: async (ctx, a) => {
      const path = arg(a, 'path', REPO_PATH).replace(/^\/+/, '');
      const ref = optArg(a, 'ref', REF) ?? 'main';
      const res = await ctx.fetch(`https://raw.githubusercontent.com/${REPO}/${encodeURI(ref)}/${encodeURI(path)}`);
      if (res.status === 404) throw new ToolError(`not found: ${path}@${ref}`);
      if (!res.ok) throw new ToolError(`github answered ${res.status}`);
      const text = await res.text();
      return { path, ref, size: text.length, truncated: text.length > MAX_TEXT, text: text.slice(0, MAX_TEXT) };
    },
  },
  {
    name: 'repo_list',
    description: `Lists a directory of the ${REPO} repository at a ref (default main).`,
    inputSchema: obj({ path: str('Directory from the repository root ("" = root).'), ref: str('Branch, tag or commit (default main).') }),
    run: async (ctx, a) => {
      const path = (optArg(a, 'path', REPO_PATH) ?? '').replace(/^\/+|\/+$/g, '');
      const ref = optArg(a, 'ref', REF) ?? 'main';
      const res = await github(ctx, `https://api.github.com/repos/${REPO}/contents/${encodeURI(path)}?ref=${encodeURIComponent(ref)}`);
      if (res.status === 404) throw new ToolError(`not found: ${path}@${ref}`);
      if (!res.ok) throw new ToolError(`github answered ${res.status}`);
      const items = (await res.json()) as { name: string; path: string; type: string; size: number }[] | { type: string };
      if (!Array.isArray(items)) throw new ToolError(`${path} is a file: use repo_file`);
      return { path, ref, entries: items.map((i) => ({ name: i.name, path: i.path, type: i.type, size: i.size })) };
    },
  },
  {
    name: 'repo_commits',
    description: `Latest commits on a branch of ${REPO} (default main): sha, date, subject.`,
    inputSchema: obj({ ref: str('Branch (default main).'), limit: { type: 'integer', minimum: 1, maximum: 30 } }),
    run: async (ctx, a) => {
      const ref = optArg(a, 'ref', REF) ?? 'main';
      const limit = typeof a.limit === 'number' ? Math.min(Math.max(Math.trunc(a.limit), 1), 30) : 10;
      const res = await github(ctx, `https://api.github.com/repos/${REPO}/commits?sha=${encodeURIComponent(ref)}&per_page=${limit}`);
      if (!res.ok) throw new ToolError(`github answered ${res.status}`);
      const list = (await res.json()) as { sha: string; commit: { message: string; author: { date: string } } }[];
      return list.map((c) => ({ sha: c.sha, date: c.commit.author.date, subject: c.commit.message.split('\n')[0] }));
    },
  },
  {
    name: 'admin_get',
    description:
      'Read-only call of any GET route of the hub admin API (services/agent-hub/README.md lists them), e.g. /admin/agents, /admin/releases, /admin/reports?tenant_id=…. Bundle downloads and the HTML status page are refused.',
    inputSchema: obj({ path: str('Path starting with /admin/, query string included.') }, ['path']),
    run: (ctx, a) => adminJson(ctx, arg(a, 'path', /^\/admin\/[A-Za-z0-9._~\-/?=&%:,+]*$/)),
  },
  {
    name: 'list_agents',
    description: 'Agents with version, channel, last heartbeat, modules and the products attached; optionally one tenant.',
    inputSchema: obj({ tenant_id: str('Tenant id (optional).') }),
    run: (ctx, a) => adminJson(ctx, `/admin/agents${qs({ tenant_id: optArg(a, 'tenant_id', ID) })}`),
  },
  {
    name: 'get_agent',
    description: 'One agent: identity, heartbeat, modules, products.',
    inputSchema: obj({ agent_id: str('Agent id.') }, ['agent_id']),
    run: (ctx, a) => adminJson(ctx, `/admin/agents/${arg(a, 'agent_id', ID)}`),
  },
  {
    name: 'agent_selftest',
    description: 'Last self-test runs an agent uploaded (pass/warn/fail per check).',
    inputSchema: obj({ agent_id: str('Agent id.') }, ['agent_id']),
    run: (ctx, a) => adminJson(ctx, `/admin/agents/${arg(a, 'agent_id', ID)}/selftest`),
  },
  {
    name: 'list_releases',
    description: 'Published releases per product and channel.',
    inputSchema: obj({}),
    run: (ctx) => adminJson(ctx, '/admin/releases'),
  },
  {
    name: 'release_gate',
    description: 'Whether a canary release may be promoted to stable, with the reasons.',
    inputSchema: obj({ product: str('Product, e.g. dl-agent.'), version: str('Version x.y.z.') }, ['product', 'version']),
    run: (ctx, a) => adminJson(ctx, `/admin/releases/${arg(a, 'product', /^[a-z][a-z0-9-]{1,31}$/)}/${arg(a, 'version', /^\d+\.\d+\.\d+$/)}/gate`),
  },
  {
    name: 'list_reports',
    description: 'Problem reports from agents, grouped by fingerprint; filter by tenant or severity.',
    inputSchema: obj({ tenant_id: str('Tenant id (optional).'), severity: { type: 'string', enum: ['info', 'warning', 'error', 'critical'] } }),
    run: (ctx, a) =>
      adminJson(ctx, `/admin/reports${qs({ tenant_id: optArg(a, 'tenant_id', ID), severity: optArg(a, 'severity', /^(info|warning|error|critical)$/) })}`),
  },
];

export const READ_TOOLS = TOOLS.map((t) => t.name);

export function toolList(): { name: string; description: string; inputSchema: Record<string, unknown> }[] {
  return TOOLS.map(({ name, description, inputSchema }) => ({ name, description, inputSchema }));
}

export async function callTool(ctx: ToolContext, name: string, args: Record<string, unknown>): Promise<unknown> {
  const tool = TOOLS.find((t) => t.name === name);
  if (!tool) throw new ToolError(`unknown tool: ${name}`);
  return tool.run(ctx, args);
}
