export interface SurveyRow {
  agent_id: string;
  received_at: number;
  body: string;
}

export function surveyStatement(db: D1Database, agentId: string, body: string, now: number): D1PreparedStatement {
  return db
    .prepare(
      `INSERT INTO agent_surveys (agent_id, received_at, body) VALUES (?1, ?2, ?3)
       ON CONFLICT (agent_id) DO UPDATE SET received_at = excluded.received_at, body = excluded.body`,
    )
    .bind(agentId, now, body);
}

export async function getSurvey(db: D1Database, agentId: string): Promise<SurveyRow | null> {
  return db.prepare('SELECT * FROM agent_surveys WHERE agent_id = ?1').bind(agentId).first<SurveyRow>();
}

export interface SiteAgentRow {
  agent_id: string;
  tenant_id: string;
  site_id: string;
  hostname: string;
  survey_at: number | null;
  survey: string | null;
}

/** Live agents with their last survey, optionally of one tenant or a set of agents. */
export async function siteAgents(db: D1Database, filter: { tenantId?: string; agentIds?: readonly string[] }): Promise<SiteAgentRow[]> {
  if (filter.agentIds && filter.agentIds.length === 0) return [];
  const where = ['a.revoked_at IS NULL'];
  const binds: unknown[] = [];
  if (filter.tenantId) {
    binds.push(filter.tenantId);
    where.push(`a.tenant_id = ?${binds.length}`);
  }
  if (filter.agentIds) {
    const marks = filter.agentIds.map((id) => {
      binds.push(id);
      return `?${binds.length}`;
    });
    where.push(`a.id IN (${marks.join(', ')})`);
  }
  const { results } = await db
    .prepare(
      `SELECT a.id AS agent_id, a.tenant_id, a.site_id, a.hostname, s.received_at AS survey_at, s.body AS survey
       FROM agents a LEFT JOIN agent_surveys s ON s.agent_id = a.id
       WHERE ${where.join(' AND ')} ORDER BY a.tenant_id, a.site_id, a.hostname`,
    )
    .bind(...binds)
    .all<SiteAgentRow>();
  return results;
}

export interface RequirementRow {
  product: string;
  body: string;
  updated_at: number;
}

export async function getRequirement(db: D1Database, product: string): Promise<RequirementRow | null> {
  return db.prepare('SELECT * FROM site_requirements WHERE product = ?1').bind(product).first<RequirementRow>();
}

export async function listRequirements(db: D1Database): Promise<RequirementRow[]> {
  return (await db.prepare('SELECT * FROM site_requirements ORDER BY product').all<RequirementRow>()).results;
}

export async function putRequirement(db: D1Database, product: string, body: string, now: number): Promise<void> {
  await db
    .prepare(
      `INSERT INTO site_requirements (product, body, updated_at) VALUES (?1, ?2, ?3)
       ON CONFLICT (product) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at`,
    )
    .bind(product, body, now)
    .run();
}

export async function deleteRequirement(db: D1Database, product: string): Promise<boolean> {
  const r = await db.prepare('DELETE FROM site_requirements WHERE product = ?1').bind(product).run();
  return (r.meta.changes ?? 0) > 0;
}
