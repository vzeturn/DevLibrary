import type { Env } from '../env';
import { limited, parseCommitFeed, parsePatch } from './githubFallback';
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

/** Directory entries of `path` from jsDelivr's flat file list of the repository (files and folders, sizes of files). */
async function listFromJsdelivr(ctx: ToolContext, path: string, ref: string): Promise<unknown> {
  const res = await ctx.fetch(`https://data.jsdelivr.com/v1/packages/gh/${REPO}@${encodeURIComponent(ref)}?structure=flat`);
  if (!res.ok) throw new ToolError(`github rate-limited and jsdelivr answered ${res.status}`);
  const { files } = (await res.json()) as { files: { name: string; size: number }[] };
  const prefix = path ? `/${path}/` : '/';
  const entries = new Map<string, { name: string; path: string; type: string; size: number }>();
  for (const f of files) {
    if (!f.name.startsWith(prefix)) continue;
    const [head, ...rest] = f.name.slice(prefix.length).split('/');
    if (!head) continue;
    const full = `${path ? `${path}/` : ''}${head}`;
    if (!entries.has(head)) entries.set(head, { name: head, path: full, type: rest.length ? 'dir' : 'file', size: rest.length ? 0 : f.size });
  }
  if (entries.size === 0) throw new ToolError(`not found: ${path}@${ref}`);
  return { path, ref, source: 'jsdelivr', entries: [...entries.values()].sort((a, b) => a.name.localeCompare(b.name)) };
}

interface GhPull {
  title: string;
  state: string;
  merged?: boolean;
  merged_at?: string | null;
  merge_commit_sha?: string | null;
  body?: string | null;
  head?: { ref: string; sha: string };
  base?: { ref: string; sha: string };
}
interface GhFile {
  filename: string;
  status: string;
  additions: number;
  deletions: number;
  patch?: string;
}
/** Longest patch excerpt per file; read the whole file with repo_file at the commit. */
export const MAX_PATCH = 4000;

function changed(files: GhFile[]): unknown[] {
  return files.map((f) => ({
    path: f.filename,
    status: f.status,
    additions: f.additions,
    deletions: f.deletions,
    patch: f.patch === undefined ? null : f.patch.length > MAX_PATCH ? `${f.patch.slice(0, MAX_PATCH)}…[truncated]` : f.patch,
  }));
}

/** The last 20 commits of `ref` from github.com's Atom feed. */
async function feed(ctx: ToolContext, ref: string): Promise<unknown[]> {
  const res = await ctx.fetch(`https://github.com/${REPO}/commits/${encodeURI(ref)}.atom`);
  if (!res.ok) throw new ToolError(`github API rate-limited and the commit feed answered ${res.status}`);
  return parseCommitFeed(await res.text()).map((c) => ({ ...c, source: 'atom' }));
}

function capped(text: string): string {
  return text.length > MAX_TEXT ? `${text.slice(0, MAX_TEXT)}…[truncated]` : text;
}

/** A pull request from its `.patch` and main's feed (merged = a "Merge pull request #n" commit there). */
async function pullFromPatch(ctx: ToolContext, n: number): Promise<unknown> {
  const res = await ctx.fetch(`https://patch-diff.githubusercontent.com/raw/${REPO}/pull/${n}.patch`);
  if (res.status === 404) throw new ToolError(`no pull request #${n}`);
  if (!res.ok) throw new ToolError(`github API rate-limited and the patch answered ${res.status}`);
  const text = await res.text();
  const { commits, files } = parsePatch(text);
  const merge = (await feed(ctx, 'main').catch(() => [] as unknown[])).find((c) =>
    String((c as { subject: string }).subject).startsWith(`Merge pull request #${n} `),
  ) as { sha: string; date: string } | undefined;
  return {
    number: n,
    source: 'patch',
    merged: merge ? true : null,
    merged_at: merge?.date ?? null,
    merge_commit: merge?.sha ?? null,
    note: merge ? undefined : 'merge state unknown from the patch: not among the last 20 commits of main',
    head: { sha: commits[commits.length - 1]?.sha ?? null },
    commits,
    files: files.map((path) => ({ path })),
    patch: capped(text),
  };
}

/** A commit from its `.patch`. */
async function commitFromPatch(ctx: ToolContext, sha: string): Promise<unknown> {
  const res = await ctx.fetch(`https://github.com/${REPO}/commit/${sha}.patch`);
  if (res.status === 404) throw new ToolError(`no commit ${sha}`);
  if (!res.ok) throw new ToolError(`github API rate-limited and the patch answered ${res.status}`);
  const text = await res.text();
  const { commits, files } = parsePatch(text);
  return { sha: commits[0]?.sha ?? sha, source: 'patch', message: commits[0]?.subject ?? '', files: files.map((path) => ({ path })), patch: capped(text) };
}

/** The sign-in facts of a survey, one entry per database (what an acceptance check reads first). */
export function surveySummary(survey: Record<string, unknown>): unknown {
  const o = (v: unknown) => (v && typeof v === 'object' ? (v as Record<string, unknown>) : {});
  const databases = Object.entries(o(survey.databases)).map(([name, raw]) => {
    const d = o(raw);
    const session = o(d.session);
    return {
      name,
      engine: o(d.engine).name ?? null,
      login: d.login ?? null,
      auth_scheme: session.auth_scheme ?? null,
      tds: session.tds ?? null,
      server_port: session.server_port ?? null,
      instance: session.instance ?? null,
      encryption: session.encryption ?? null,
      tls: session.tls ?? d.transport ?? null,
      warnings: Array.isArray(d.warnings) ? d.warnings : [],
    };
  });
  return { login_source: survey.login_source ?? survey.sql_source ?? null, host: o(survey.host).version ?? null, databases };
}

const TOOLS: ToolDef[] = [
  {
    name: 'hub_test_results',
    description: 'Latest published test runs: suite agent-hub (pnpm typecheck + vitest) or cargo (clippy + cargo test --workspace of the Rust crates, any commit). Commit, counts, typecheck (clippy for cargo), failures. Newest first.',
    inputSchema: obj({ suite: str('Suite name: agent-hub (pnpm typecheck + vitest, default) or cargo (cargo clippy -D warnings as typecheck + cargo test --workspace, run isolated: no secrets, no network).'), limit: { type: 'integer', minimum: 1, maximum: 20, description: 'Runs to return (default 1).' } }),
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
      // The unauthenticated API is rate-limited per IP, and Worker IPs are shared: list from jsDelivr then.
      if (res.status === 403 || res.status === 429) return listFromJsdelivr(ctx, path, ref);
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
      if (limited(res.status)) return (await feed(ctx, ref)).slice(0, limit);
      if (!res.ok) throw new ToolError(`github answered ${res.status}`);
      const list = (await res.json()) as { sha: string; commit: { message: string; author: { date: string } } }[];
      return list.map((c) => ({ sha: c.sha, date: c.commit.author.date, subject: c.commit.message.split('\n')[0] }));
    },
  },
  {
    name: 'repo_pull',
    description: `One pull request of ${REPO}: title, state, merged, head and base commits, description, and the files it changes (status, line counts, patch excerpt).`,
    inputSchema: obj({ number: { type: 'integer', minimum: 1, description: 'Pull request number, e.g. 65.' } }, ['number']),
    run: async (ctx, a) => {
      const n = typeof a.number === 'number' && Number.isInteger(a.number) && a.number > 0 && a.number < 1e7 ? a.number : 0;
      if (!n) throw new ToolError('invalid number');
      const res = await github(ctx, `https://api.github.com/repos/${REPO}/pulls/${n}`);
      if (limited(res.status)) return pullFromPatch(ctx, n);
      if (res.status === 404) throw new ToolError(`no pull request #${n}`);
      if (!res.ok) throw new ToolError(`github answered ${res.status}`);
      const pr = (await res.json()) as GhPull;
      const files = await github(ctx, `https://api.github.com/repos/${REPO}/pulls/${n}/files?per_page=100`);
      return {
        number: n,
        title: pr.title,
        state: pr.state,
        merged: pr.merged ?? Boolean(pr.merged_at),
        merged_at: pr.merged_at ?? null,
        merge_commit: pr.merge_commit_sha ?? null,
        head: { ref: pr.head?.ref, sha: pr.head?.sha },
        base: { ref: pr.base?.ref, sha: pr.base?.sha },
        body: (pr.body ?? '').slice(0, 20_000),
        files: files.ok ? changed((await files.json()) as GhFile[]) : { error: `github answered ${files.status}` },
      };
    },
  },
  {
    name: 'repo_commit',
    description: `One commit of ${REPO} (full or short sha): message, author date, parents and the files it changes (status, line counts, patch excerpt).`,
    inputSchema: obj({ sha: str('Commit sha, e.g. f241daf.') }, ['sha']),
    run: async (ctx, a) => {
      const sha = arg(a, 'sha', /^[0-9a-f]{7,40}$/);
      const res = await github(ctx, `https://api.github.com/repos/${REPO}/commits/${sha}`);
      if (limited(res.status)) return commitFromPatch(ctx, sha);
      if (res.status === 404 || res.status === 422) throw new ToolError(`no commit ${sha}`);
      if (!res.ok) throw new ToolError(`github answered ${res.status}`);
      const c = (await res.json()) as { sha: string; commit: { message: string; author: { date: string } }; parents?: { sha: string }[]; files?: GhFile[] };
      return { sha: c.sha, date: c.commit.author.date, message: c.commit.message.slice(0, 20_000), parents: (c.parents ?? []).map((p) => p.sha), files: changed(c.files ?? []) };
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
    inputSchema: obj({ tenant_id: str('Tenant id (optional).'), include_revoked: str('"1" also lists revoked agents (default: live machines only).') }),
    run: (ctx, a) =>
      adminJson(ctx, `/admin/agents${qs({ tenant_id: optArg(a, 'tenant_id', ID), include_revoked: optArg(a, 'include_revoked', /^(1|true)$/) })}`),
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
    name: 'agent_survey',
    description:
      'Last site survey an agent sent (PROTOCOL §9.5; facts, never a secret), with a summary first: where its SQL login came from (login_source / sql_source) and, per database, how it signed in (session.auth_scheme, tds, server_port, instance, encryption, tls), engine and warnings.',
    inputSchema: obj({ agent_id: str('Agent id.') }, ['agent_id']),
    run: async (ctx, a) => {
      const res = (await adminJson(ctx, `/admin/agents/${arg(a, 'agent_id', ID)}/survey`)) as { status: number; body: unknown };
      if (res.status !== 200) return res;
      const body = res.body as { received_at?: number; survey?: Record<string, unknown> };
      return { received_at: body.received_at ?? null, summary: surveySummary(body.survey ?? {}), survey: body.survey ?? null };
    },
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
