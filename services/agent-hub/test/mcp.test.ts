import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { app } from '../src/index';
import { READ_TOOLS } from '../src/services/mcpTools';
import { admin, enrollAgent, setNow, T0, testEnv, type TestEnv } from './helpers';

const MCP = 'test-mcp-token';
let env: TestEnv;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv({ MCP_TOKEN: MCP });
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function rpc(body: unknown, token: string | null = MCP): Promise<Response> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  return Promise.resolve(app.request('https://hub.test/mcp', { method: 'POST', headers, body: JSON.stringify(body) }, env));
}

async function call(name: string, args: Record<string, unknown> = {}): Promise<{ text: string; isError?: boolean; json: () => unknown }> {
  const res = await rpc({ jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name, arguments: args } });
  expect(res.status).toBe(200);
  const out = (await res.json()) as { result: { content: { text: string }[]; isError?: boolean } };
  const text = out.result.content[0]?.text ?? '';
  return { text, isError: out.result.isError, json: () => JSON.parse(text) as unknown };
}

const RUN = {
  commit: 'abcdef1234567',
  branch: 'main',
  ran_at: T0 - 60,
  duration_ms: 4200,
  command: 'pnpm test',
  runner: 'cloud',
  typecheck: 'pass',
  files: { passed: 24, failed: 0, skipped: 0 },
  tests: { passed: 230, failed: 0, skipped: 10 },
  failures: [],
};

describe('/mcp ops console', () => {
  it('needs its own token; the admin token or none is refused, and no token configured locks it', async () => {
    expect((await rpc({ jsonrpc: '2.0', id: 1, method: 'ping' }, null)).status).toBe(401);
    expect((await rpc({ jsonrpc: '2.0', id: 1, method: 'ping' }, 'test-admin-token')).status).toBe(401);
    env = testEnv();
    expect((await rpc({ jsonrpc: '2.0', id: 1, method: 'ping' })).status).toBe(401);
  });

  it('speaks MCP: initialize, notifications, tools/list with read-only tools, unknown method', async () => {
    const init = (await (await rpc({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18' } })).json()) as {
      result: { protocolVersion: string; capabilities: unknown };
    };
    expect(init.result).toMatchObject({ protocolVersion: '2025-06-18', capabilities: { tools: {} } });
    expect((await rpc({ jsonrpc: '2.0', method: 'notifications/initialized' })).status).toBe(202);
    const list = (await (await rpc({ jsonrpc: '2.0', id: 2, method: 'tools/list' })).json()) as { result: { tools: { name: string }[] } };
    expect(list.result.tools.map((t) => t.name)).toEqual(READ_TOOLS);
    expect(READ_TOOLS).toEqual(expect.arrayContaining(['hub_test_results', 'repo_file', 'repo_list', 'admin_get']));
    const unknown = (await (await rpc({ jsonrpc: '2.0', id: 3, method: 'resources/list' })).json()) as { error: { code: number } };
    expect(unknown.error.code).toBe(-32601);
    expect((await rpc('not json at all')).status).toBe(400);
  });

  it('hub_test_results reads the runs published with the admin API, newest first', async () => {
    expect(await (await call('hub_test_results')).json()).toMatchObject({ runs: [], note: expect.stringContaining('test:publish') });
    expect((await admin(env, 'PUT', '/admin/test-runs/agent-hub', RUN)).status).toBe(201);
    const failed = { ...RUN, commit: 'bbbbbbb', tests: { passed: 229, failed: 1, skipped: 10 }, failures: [{ file: 'test/x.test.ts', name: 'x > y', message: 'expected 1 to be 2' }] };
    expect(await (await admin(env, 'PUT', '/admin/test-runs/agent-hub', failed)).json()).toMatchObject({ ok: false });
    const out = (await (await call('hub_test_results', { limit: 2 })).json()) as { runs: { commit: string; ok: boolean }[] };
    expect(out.runs.map((r) => [r.commit, r.ok])).toEqual([
      ['bbbbbbb', false],
      ['abcdef1234567', true],
    ]);
    expect((await admin(env, 'PUT', '/admin/test-runs/agent-hub', { ...RUN, commit: 'not-a-sha' })).status).toBe(400);
    expect((await admin(env, 'PUT', '/admin/test-runs/Bad Suite', RUN)).status).toBe(400);
  });

  it('keeps the last 20 runs of a suite', async () => {
    for (let i = 0; i < 22; i++) await admin(env, 'PUT', '/admin/test-runs/agent-hub', { ...RUN, ran_at: T0 + i });
    expect(env.DB.raw.prepare("SELECT COUNT(*) AS n FROM test_runs WHERE suite = 'agent-hub'").get()).toEqual({ n: 20 });
  });

  it('admin_get and the named tools read the admin API; writes, downloads and other paths are refused', async () => {
    const agent = await enrollAgent(env, 'mnb');
    const audited = env.DB.raw.prepare('SELECT COUNT(*) AS n FROM admin_audit').get();
    const agents = (await (await call('list_agents', { tenant_id: 'mnb' })).json()) as { status: number; body: { agents: { id: string }[] } };
    expect(agents.status).toBe(200);
    expect(agents.body.agents.map((a) => a.id)).toEqual([agent.id]);
    expect(((await (await call('get_agent', { agent_id: agent.id })).json()) as { status: number }).status).toBe(200);
    expect(((await (await call('admin_get', { path: '/admin/releases' })).json()) as { status: number }).status).toBe(200);
    for (const path of ['/admin/diagnostics/x/download', '/admin/', '/admin', '/v1/heartbeat', '/admin/../v1/x']) {
      expect((await call('admin_get', { path })).isError, path).toBe(true);
    }
    expect((await call('get_agent', { agent_id: '../tenants' })).isError).toBe(true);
    expect((await call('no_such_tool')).isError).toBe(true);
    // Nothing reached the audit trail: every call was a GET.
    expect(env.DB.raw.prepare('SELECT COUNT(*) AS n FROM admin_audit').get()).toEqual(audited);
  });

  it('repo_file, repo_list and repo_commits read the public repository on main', async () => {
    const seen: string[] = [];
    vi.stubGlobal('fetch', async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.includes('raw.githubusercontent.com') && url.endsWith('/README.md')) return new Response('# DevLibrary\n');
      if (url.includes('/contents/crates')) return Response.json([{ name: 'dl-agent', path: 'crates/dl-agent', type: 'dir', size: 0, sha: 'x' }]);
      if (url.includes('/commits?')) return Response.json([{ sha: 'abc', commit: { message: 'feat: x\n\nbody', author: { date: '2026-11-20T00:00:00Z' } } }]);
      return new Response('nope', { status: 404 });
    });
    expect(await (await call('repo_file', { path: 'README.md' })).json()).toMatchObject({ ref: 'main', text: '# DevLibrary\n' });
    expect(seen[0]).toBe('https://raw.githubusercontent.com/vzeturn/DevLibrary/main/README.md');
    expect(await (await call('repo_list', { path: 'crates' })).json()).toMatchObject({ entries: [{ name: 'dl-agent', type: 'dir' }] });
    expect(await (await call('repo_commits', { limit: 1 })).json()).toEqual([{ sha: 'abc', date: '2026-11-20T00:00:00Z', subject: 'feat: x' }]);
    expect((await call('repo_file', { path: 'missing.md' })).isError).toBe(true);
    // Rate-limited API: the listing comes from jsDelivr's flat file list.
    vi.stubGlobal('fetch', async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.startsWith('https://api.github.com/')) return new Response('rate limited', { status: 403 });
      if (url.startsWith('https://data.jsdelivr.com/v1/packages/gh/vzeturn/DevLibrary@main')) {
        return Response.json({ files: [{ name: '/crates/dl-agent/README.md', size: 9 }, { name: '/crates/dl-agent/src/lib.rs', size: 5 }, { name: '/crates/Cargo.toml', size: 3 }, { name: '/README.md', size: 1 }] });
      }
      return new Response('nope', { status: 404 });
    });
    expect(await (await call('repo_list', { path: 'crates' })).json()).toEqual({
      path: 'crates',
      ref: 'main',
      source: 'jsdelivr',
      entries: [
        { name: 'Cargo.toml', path: 'crates/Cargo.toml', type: 'file', size: 3 },
        { name: 'dl-agent', path: 'crates/dl-agent', type: 'dir', size: 0 },
      ],
    });
    expect((await call('repo_list', { path: 'nothing' })).isError).toBe(true);
    expect((await call('repo_file', { path: '../../etc/passwd' })).isError).toBe(true);
  });

  it('repo_pull and repo_commit read one pull request and one commit, patches capped', async () => {
    const seen: string[] = [];
    vi.stubGlobal('fetch', async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.endsWith('/pulls/65')) {
        return Response.json({ title: 'feat: x', state: 'closed', merged: true, merged_at: '2026-11-19T00:00:00Z', merge_commit_sha: 'm1', body: 'why', head: { ref: 'feat', sha: 'h1' }, base: { ref: 'main', sha: 'b1' } });
      }
      if (url.includes('/pulls/65/files')) return Response.json([{ filename: 'docs/report.md', status: 'added', additions: 3, deletions: 0, patch: 'x'.repeat(5000) }]);
      if (url.endsWith('/commits/f241daf')) {
        return Response.json({ sha: 'f241daf000', commit: { message: 'fix: y', author: { date: '2026-11-19T01:00:00Z' } }, parents: [{ sha: 'p1' }], files: [{ filename: 'a.rs', status: 'modified', additions: 1, deletions: 1, patch: '@@ -1 +1 @@' }] });
      }
      return new Response('nope', { status: 404 });
    });
    const pr = (await (await call('repo_pull', { number: 65 })).json()) as { merged: boolean; head: { sha: string }; files: { path: string; patch: string }[] };
    expect(pr).toMatchObject({ number: 65, merged: true, head: { sha: 'h1' }, base: { ref: 'main' } });
    expect(pr.files[0]?.path).toBe('docs/report.md');
    expect(pr.files[0]?.patch.endsWith('…[truncated]')).toBe(true);
    expect(seen).toContain('https://api.github.com/repos/vzeturn/DevLibrary/pulls/65/files?per_page=100');
    expect(await (await call('repo_commit', { sha: 'f241daf' })).json()).toMatchObject({ sha: 'f241daf000', message: 'fix: y', parents: ['p1'], files: [{ path: 'a.rs', patch: '@@ -1 +1 @@' }] });
    expect((await call('repo_pull', { number: 9 })).isError).toBe(true);
    expect((await call('repo_pull', { number: 'x' })).isError).toBe(true);
    expect((await call('repo_commit', { sha: 'main; rm' })).isError).toBe(true);
  });

  it('falls back to the commit feed and .patch files when the GitHub API is rate-limited', async () => {
    const atom = (n: number, sha: string, subject: string) =>
      `<entry><id>tag:github.com,2008:Grit::Commit/${sha}</id><title>\n  ${subject}\n</title><updated>2026-11-20T0${n}:00:00Z</updated></entry>`;
    const feed = `<feed>${atom(2, 'e'.repeat(40), 'Merge pull request #65 from vzeturn/cloud/rt-0258')}${atom(1, 'f'.repeat(40), 'fix: a &amp; b')}</feed>`;
    const patch = [
      `From ${'a'.repeat(40)} Mon Sep 17 00:00:00 2001`,
      'Subject: [PATCH 1/2] feat(dl-log): scrub URL credentials, JWTs and more secret',
      ' field names',
      '---',
      'diff --git a/crates/dl-log/README.md b/crates/dl-log/README.md',
      `From ${'b'.repeat(40)} Mon Sep 17 00:00:00 2001`,
      'Subject: [PATCH 2/2] docs: report',
      'diff --git a/docs/plans/report.md b/docs/plans/report.md',
    ].join('\n');
    const seen: string[] = [];
    vi.stubGlobal('fetch', async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.startsWith('https://api.github.com/')) return new Response('rate limited', { status: 403 });
      if (url === 'https://github.com/vzeturn/DevLibrary/commits/main.atom') return new Response(feed);
      if (url === 'https://patch-diff.githubusercontent.com/raw/vzeturn/DevLibrary/pull/65.patch') return new Response(patch);
      if (url === 'https://github.com/vzeturn/DevLibrary/commit/f241daf.patch') return new Response(patch);
      return new Response('nope', { status: 404 });
    });
    expect(await (await call('repo_commits', { limit: 2 })).json()).toEqual([
      { sha: 'e'.repeat(40), date: '2026-11-20T02:00:00Z', subject: 'Merge pull request #65 from vzeturn/cloud/rt-0258', source: 'atom' },
      { sha: 'f'.repeat(40), date: '2026-11-20T01:00:00Z', subject: 'fix: a & b', source: 'atom' },
    ]);
    expect(await (await call('repo_pull', { number: 65 })).json()).toMatchObject({
      number: 65,
      source: 'patch',
      merged: true,
      merge_commit: 'e'.repeat(40),
      head: { sha: 'b'.repeat(40) },
      commits: [
        { sha: 'a'.repeat(40), subject: 'feat(dl-log): scrub URL credentials, JWTs and more secret field names' },
        { sha: 'b'.repeat(40), subject: 'docs: report' },
      ],
      files: [{ path: 'crates/dl-log/README.md' }, { path: 'docs/plans/report.md' }],
    });
    expect(await (await call('repo_commit', { sha: 'f241daf' })).json()).toMatchObject({ source: 'patch', sha: 'a'.repeat(40), files: [{ path: 'crates/dl-log/README.md' }, { path: 'docs/plans/report.md' }] });
    expect((await call('repo_pull', { number: 9 })).isError).toBe(true);
    expect(seen.every((u) => !u.includes('token'))).toBe(true);
  });
});
