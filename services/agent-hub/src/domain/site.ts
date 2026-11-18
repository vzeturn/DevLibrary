// Site readiness (PROTOCOL §9.5): a product's requirements checked against an agent's last survey.
import { z } from 'zod';

/** Grants the survey reports per database; `sysadmin` and `db_owner` satisfy every one. */
export const grantName = z.enum([
  'sysadmin',
  'db_owner',
  'create_procedure',
  'create_function',
  'create_table',
  'alter_dbo',
  'execute',
  'view_definition',
  'backup_database',
]);

const databaseRequirement = z.object({
  /** Oldest SQL Server major version: 9 = 2005, 10 = 2008 / 2008 R2, 11 = 2012. */
  min_engine_major: z.number().int().min(8).max(40).optional(),
  min_compatibility: z.number().int().min(80).max(200).optional(),
  grants: z.array(grantName).max(20).default([]),
  /** Survey warnings that make the site not ready (`database_read_only`, `express_edition` …). */
  forbid_warnings: z.array(z.string().regex(/^[a-z0-9_]{1,64}$/)).max(20).default([]),
  /** The last schema probe (§9.6) must have found nothing missing. */
  probe_ok: z.boolean().default(false),
});

export const siteRequirement = z.object({
  host: z
    .object({
      /** Oldest Windows NT version, `"6.1"` = Windows 7 / Server 2008 R2. */
      min_nt_version: z.string().regex(/^\d{1,2}\.\d{1,2}$/).optional(),
      /** Allowed OS families (`windows`, `linux`). */
      families: z.array(z.string().regex(/^[a-z]{1,20}$/)).max(5).optional(),
    })
    .default({}),
  /** Declared database name (as in the agent's `sql.json`) → what it needs. */
  databases: z.record(z.string().regex(/^[A-Za-z0-9._-]{1,64}$/), databaseRequirement).default({}),
});
export type SiteRequirement = z.infer<typeof siteRequirement>;

/** A survey older than this no longer proves anything (agents re-send it at least daily). */
export const SURVEY_STALE_SECONDS = 3 * 86400;

export interface SiteReason {
  code: string;
  database?: string;
  detail: string;
}

export interface SiteCheck {
  eligible: boolean;
  reasons: SiteReason[];
}

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj => (v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Obj) : {});
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);

function ntBelow(actual: string, min: string): boolean {
  const a = actual.split('.').map(Number);
  const m = min.split('.').map(Number);
  for (let i = 0; i < m.length; i++) {
    const x = a[i] ?? 0;
    const y = m[i] ?? 0;
    if (x !== y) return x < y;
  }
  return false;
}

/** Whether a site meets `req`, from the survey received at `surveyAt` (`null`: none yet). */
export function checkSite(req: SiteRequirement, survey: unknown, surveyAt: number | null, now: number): SiteCheck {
  if (survey === null || survey === undefined || surveyAt === null) {
    return { eligible: false, reasons: [{ code: 'no_survey', detail: 'the agent has not sent a site survey yet' }] };
  }
  const reasons: SiteReason[] = [];
  if (now - surveyAt > SURVEY_STALE_SECONDS) {
    reasons.push({ code: 'survey_stale', detail: `last survey ${Math.floor((now - surveyAt) / 86400)} days old` });
  }
  const host = obj(obj(survey).host);
  const family = typeof host.family === 'string' ? host.family : null;
  if (req.host.families && !(family !== null && req.host.families.includes(family))) {
    reasons.push({ code: 'os_family', detail: `os ${family ?? 'unknown'} not in ${req.host.families.join(', ')}` });
  }
  if (req.host.min_nt_version && family === 'windows') {
    const nt = typeof host.nt_version === 'string' ? host.nt_version : null;
    if (nt === null || ntBelow(nt, req.host.min_nt_version)) {
      reasons.push({ code: 'windows_too_old', detail: `Windows NT ${nt ?? 'unknown'} < ${req.host.min_nt_version}` });
    }
  }

  const databases = obj(obj(survey).databases);
  for (const [name, need] of Object.entries(req.databases)) {
    const db = databases[name];
    if (db === undefined) {
      reasons.push({ code: 'database_not_surveyed', database: name, detail: `${name}: not declared or never reached` });
      continue;
    }
    const d = obj(db);
    const major = num(obj(d.engine).major);
    if (need.min_engine_major !== undefined && (major === null || major < need.min_engine_major)) {
      reasons.push({ code: 'engine_too_old', database: name, detail: `${name}: SQL Server major ${major ?? '?'} < ${need.min_engine_major}` });
    }
    const compat = num(obj(d.database).compatibility_level);
    if (need.min_compatibility !== undefined && (compat === null || compat < need.min_compatibility)) {
      reasons.push({ code: 'compatibility_too_low', database: name, detail: `${name}: compat ${compat ?? '?'} < ${need.min_compatibility}` });
    }
    const grants = obj(d.grants);
    const owner = grants.sysadmin === true || grants.db_owner === true;
    const missing = owner ? [] : need.grants.filter((g) => grants[g] !== true);
    if (missing.length) {
      reasons.push({ code: 'grant_missing', database: name, detail: `${name}: login lacks ${missing.join(', ')}` });
    }
    const warnings = Array.isArray(d.warnings) ? d.warnings.filter((w): w is string => typeof w === 'string') : [];
    const forbidden = need.forbid_warnings.filter((w) => warnings.includes(w));
    if (forbidden.length) {
      reasons.push({ code: 'warning', database: name, detail: `${name}: ${forbidden.join(', ')}` });
    }
    if (need.probe_ok) {
      const probe = d.probe;
      if (probe === undefined) {
        reasons.push({ code: 'probe_absent', database: name, detail: `${name}: schema not probed yet (run sql.probe)` });
      } else if (obj(probe).ok !== true) {
        const items = Array.isArray(obj(probe).missing) ? (obj(probe).missing as unknown[]) : [];
        const list = items
          .slice(0, 5)
          .map((m) => `${String(obj(m).kind ?? '?')} ${String(obj(m).name ?? '?')}`)
          .join(', ');
        reasons.push({ code: 'schema_missing', database: name, detail: `${name}: missing ${items.length} (${list})` });
      }
    }
  }
  return { eligible: reasons.length === 0, reasons };
}
