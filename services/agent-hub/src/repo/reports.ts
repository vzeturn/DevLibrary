import type { ProblemReport } from '../domain/schemas';

export interface ReportRow {
  agent_id: string;
  fingerprint: string;
  tenant_id: string;
  severity: string;
  module: string;
  code: string;
  command: string;
  message: string;
  context: string;
  first_seen: number;
  last_seen: number;
  count: number;
  agent_version: string;
  os: string | null;
  hostname: string | null;
  received_first_at: number;
  received_last_at: number;
}

export interface ReportFilter {
  tenant_id?: string;
  severity?: string;
  fingerprint?: string;
  version?: string;
  limit: number;
}

// Positions in this list rank severities so the strongest one seen is kept per version.
const RANK = `'info,warning,error,critical'`;

/** Upserts by (agent_id, fingerprint): counts add up, first_seen stays, the latest version wins. */
export async function upsertReports(
  db: D1Database,
  agentId: string,
  tenantId: string,
  reports: ProblemReport[],
  now: number,
): Promise<void> {
  if (reports.length === 0) return;
  const stmts = reports.flatMap((r) => [
    db
      .prepare(
        `INSERT INTO reports (agent_id, fingerprint, tenant_id, severity, module, code, command, message, context,
                              first_seen, last_seen, count, agent_version, os, hostname,
                              received_first_at, received_last_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16)
         ON CONFLICT (agent_id, fingerprint) DO UPDATE SET
           count = reports.count + excluded.count,
           last_seen = MAX(reports.last_seen, excluded.last_seen),
           severity = excluded.severity, module = excluded.module, code = excluded.code,
           command = excluded.command,
           message = excluded.message, context = excluded.context,
           agent_version = excluded.agent_version, os = excluded.os, hostname = excluded.hostname,
           received_last_at = excluded.received_last_at`,
      )
      .bind(
        agentId,
        r.fingerprint,
        tenantId,
        r.severity,
        r.module,
        r.code,
        r.command,
        r.message,
        JSON.stringify(r.context),
        r.first_seen,
        r.last_seen,
        r.count,
        r.agent_version,
        r.os ?? null,
        r.hostname ?? null,
        now,
      ),
    db
      .prepare(
        `INSERT INTO report_versions (fingerprint, version, severity, first_received_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (fingerprint, version) DO UPDATE SET severity = CASE
           WHEN instr(${RANK}, excluded.severity) > instr(${RANK}, report_versions.severity)
           THEN excluded.severity ELSE report_versions.severity END`,
      )
      .bind(r.fingerprint, r.agent_version, r.severity, now),
  ]);
  await db.batch(stmts);
}

export async function listReports(db: D1Database, f: ReportFilter): Promise<ReportRow[]> {
  const sql = `SELECT * FROM reports
    WHERE (?1 IS NULL OR tenant_id = ?1) AND (?2 IS NULL OR severity = ?2)
      AND (?3 IS NULL OR fingerprint = ?3) AND (?4 IS NULL OR agent_version = ?4)
    ORDER BY received_last_at DESC LIMIT ?5`;
  const res = await db
    .prepare(sql)
    .bind(f.tenant_id ?? null, f.severity ?? null, f.fingerprint ?? null, f.version ?? null, f.limit)
    .all<ReportRow>();
  return res.results;
}
