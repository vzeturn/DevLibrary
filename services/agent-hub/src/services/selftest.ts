import type { AgentRow, Env } from '../env';
import type { SelftestBody, SelftestStatus } from '../domain/schemas';
import { uuidv7 } from '../lib/crypto';
import { HttpError } from '../lib/http';
import { getAgent } from '../repo/agents';
import { latestSelftest, saveSelftest, selftestHistory, type SelftestRow } from '../repo/selftest';

export interface SelftestSummary {
  pass: number;
  warn: number;
  fail: number;
  skip: number;
  total: number;
  os: string | null;
  hostname: string | null;
}

export type SelftestCheck = SelftestBody['checks'][number];

export function summarise(body: SelftestBody): SelftestSummary {
  const counts: Record<SelftestStatus, number> = { pass: 0, warn: 0, fail: 0, skip: 0 };
  for (const c of body.checks) counts[c.status] += 1;
  return { ...counts, total: body.checks.length, os: body.os ?? null, hostname: body.hostname ?? null };
}

export async function storeSelftest(env: Env, agent: AgentRow, body: SelftestBody, now: number): Promise<{ accepted: true; run_id: string }> {
  await saveSelftest(env.DB, {
    id: uuidv7(now * 1000),
    agent_id: agent.id,
    run_id: body.run_id,
    trigger: body.trigger,
    started_at: body.started_at,
    finished_at: body.finished_at,
    agent_version: body.agent_version,
    summary_json: JSON.stringify(summarise(body)),
    checks_json: JSON.stringify(body.checks),
    received_at: now,
  });
  return { accepted: true, run_id: body.run_id };
}

interface RunView {
  id: string;
  run_id: string;
  trigger: string;
  started_at: number;
  finished_at: number;
  agent_version: string;
  received_at: number;
  summary: SelftestSummary;
}

function runView(r: Omit<SelftestRow, 'checks_json'>): RunView {
  return {
    id: r.id,
    run_id: r.run_id,
    trigger: r.trigger,
    started_at: r.started_at,
    finished_at: r.finished_at,
    agent_version: r.agent_version,
    received_at: r.received_at,
    summary: JSON.parse(r.summary_json) as SelftestSummary,
  };
}

/** Latest run with its checks, and the summaries of every kept run (newest first). */
export async function selftestView(
  env: Env,
  agentId: string,
): Promise<{ agent_id: string; latest: (RunView & { checks: SelftestCheck[] }) | null; history: RunView[] }> {
  if (!(await getAgent(env.DB, agentId))) throw new HttpError(404, 'agent_not_found');
  const latest = await latestSelftest(env.DB, agentId);
  const history = (await selftestHistory(env.DB, agentId)).map(runView);
  return {
    agent_id: agentId,
    latest: latest ? { ...runView(latest), checks: JSON.parse(latest.checks_json) as SelftestCheck[] } : null,
    history,
  };
}
