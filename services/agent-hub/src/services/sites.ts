import type { Env } from '../env';
import { checkSite, siteRequirement, type SiteReason, type SiteRequirement } from '../domain/site';
import { HttpError, validate } from '../lib/http';
import { deleteRequirement, getRequirement, getSurvey, putRequirement, siteAgents, type SiteAgentRow } from '../repo/sites';

export interface SiteView {
  agent_id: string;
  tenant_id: string;
  site_id: string;
  hostname: string;
  survey_at: number | null;
  eligible: boolean;
  reasons: SiteReason[];
}

export async function requirementOf(env: Env, product: string): Promise<SiteRequirement | null> {
  const row = await getRequirement(env.DB, product);
  return row ? validate(siteRequirement, JSON.parse(row.body)) : null;
}

export async function setRequirement(env: Env, product: string, body: unknown, now: number): Promise<SiteRequirement> {
  const req = validate(siteRequirement, body);
  await putRequirement(env.DB, product, JSON.stringify(req), now);
  return req;
}

export async function removeRequirement(env: Env, product: string): Promise<void> {
  if (!(await deleteRequirement(env.DB, product))) throw new HttpError(404, 'no_site_requirement');
}

function view(req: SiteRequirement, row: SiteAgentRow, now: number): SiteView {
  const survey = row.survey === null ? null : (JSON.parse(row.survey) as unknown);
  const { eligible, reasons } = checkSite(req, survey, row.survey_at, now);
  const { agent_id, tenant_id, site_id, hostname, survey_at } = row;
  return { agent_id, tenant_id, site_id, hostname, survey_at, eligible, reasons };
}

/** Readiness of every live agent's site (or those listed) for one product. */
export async function siteReadiness(
  env: Env,
  req: SiteRequirement,
  filter: { tenantId?: string; agentIds?: readonly string[] },
  now: number,
): Promise<SiteView[]> {
  return (await siteAgents(env.DB, filter)).map((row) => view(req, row, now));
}

export async function surveyOf(env: Env, agentId: string): Promise<{ received_at: number; survey: unknown }> {
  const row = await getSurvey(env.DB, agentId);
  if (!row) throw new HttpError(404, 'no_survey');
  return { received_at: row.received_at, survey: JSON.parse(row.body) as unknown };
}
