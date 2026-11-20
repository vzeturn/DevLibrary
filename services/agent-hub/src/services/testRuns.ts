import { z } from 'zod';
import type { Env } from '../env';
import { insertTestRun, listTestRuns } from '../repo/testRuns';

export const MAX_TEST_RUN_BYTES = 128 * 1024;
export const suiteName = z.string().regex(/^[a-z][a-z0-9-]{1,40}$/, 'lowercase letters, digits and - only');

const counts = z.object({ passed: z.number().int().min(0), failed: z.number().int().min(0), skipped: z.number().int().min(0) });

/** What `scripts/publish-test-run.mjs` sends after `pnpm test` (counts and failures only). */
export const testRunBody = z.object({
  commit: z.string().regex(/^[0-9a-f]{7,40}$/),
  branch: z.string().max(200).optional(),
  ran_at: z.number().int().positive(),
  duration_ms: z.number().int().min(0).optional(),
  command: z.string().max(200),
  runner: z.string().max(100),
  typecheck: z.enum(['pass', 'fail', 'not_run']).default('not_run'),
  files: counts,
  tests: counts,
  failures: z
    .array(z.object({ file: z.string().max(300), name: z.string().max(500), message: z.string().max(2000) }))
    .max(50)
    .default([]),
});
export type TestRunBody = z.infer<typeof testRunBody>;

export interface TestRunView extends TestRunBody {
  id: number;
  suite: string;
  ok: boolean;
  received_at: number;
}

export async function storeTestRun(env: Env, suite: string, body: TestRunBody, now: number): Promise<{ id: number; ok: boolean }> {
  const ok = body.tests.failed === 0 && body.files.failed === 0 && body.typecheck !== 'fail';
  const id = await insertTestRun(env.DB, {
    suite,
    commit_sha: body.commit,
    ran_at: body.ran_at,
    received_at: now,
    ok: ok ? 1 : 0,
    body: JSON.stringify(body),
  });
  return { id, ok };
}

export async function testRunsView(env: Env, suite: string, limit: number): Promise<TestRunView[]> {
  return (await listTestRuns(env.DB, suite, limit)).map((r) => ({
    ...(JSON.parse(r.body) as TestRunBody),
    id: r.id,
    suite: r.suite,
    ok: r.ok === 1,
    received_at: r.received_at,
  }));
}
