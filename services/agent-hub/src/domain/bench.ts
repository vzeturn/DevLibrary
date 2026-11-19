import { z } from 'zod';

/** No control characters: every value ends up inside a PowerShell string literal. */
const plain = (max: number) => z.string().max(max).regex(/^[^\u0000-\u001f\u007f]*$/, 'no control characters');
const id = z.string().regex(/^[A-Za-z0-9._-]{1,64}$/, 'letters, digits, . _ - only');
/** dl-agent `sql-add` key rule; the key is also the database name on the server. */
export const sqlName = z.string().regex(/^[A-Za-z0-9_-]{1,64}$/, 'letters, digits, _ - only');

/** Commands the bench script runs itself or that would undo it. */
const RESERVED = new Set(['enroll', 'install', 'uninstall', 'run', 'watchdog', 'sql-add']);

export const SQL_PLACEHOLDER = /\{\{SQL:([^{}]*)\}\}/g;
/**
 * The prompted SQL Server connection field by field (env only), for tools that take them apart
 * (E-Invoice: `MMEI_SQL_HOST`, `MMEI_SQL_USER`, `MMEI_SQL_PASSWORD`, `MMEI_SQL_AUTH`).
 * `SQL_AUTH` is `sql` or `windows`; with `windows`, user and password are empty.
 * `SQL_PASSWORD` is a secret: masked in the console and the log like every env value.
 */
export const SQL_PARTS = ['SQL_SERVER', 'SQL_USER', 'SQL_PASSWORD', 'SQL_AUTH'] as const;
const SQL_PART = new Set<string>(SQL_PARTS);
/** Any `{{…}}`: only `{{BENCH_DIR}}` (args and env), `{{SQL:<DbName>}}` and the `SQL_PARTS` (env only) exist. */
const ANY_PLACEHOLDER = /\{\{([^{}]*)\}\}/g;
export const BENCH_DIR = 'BENCH_DIR';

/** SQL text may span lines and hold tabs; other control characters are refused. */
const sqlText = z
  .string()
  .min(1)
  .max(200_000)
  .regex(/^[^\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]*$/, 'no control characters besides tab and line breaks');

/**
 * A SQL script run once, in one transaction, against a database of the prompted SQL Server before
 * dl-agent is installed (e.g. disable the restored copy's outbound triggers). `GO` lines split batches.
 */
export const sqlFixup = z
  .object({
    /** What the console and log show; the SQL itself is never printed. */
    name: plain(100).min(1).optional(),
    database: sqlName,
    sql_url: plain(2000).regex(/^https:\/\/[^\s'"]+$/, 'an https URL').optional(),
    /** Hex SHA-256 the downloaded `sql_url` body must match. */
    sha256: z.string().regex(/^[0-9a-f]{64}$/, 'hex SHA-256').optional(),
    sql_inline: sqlText.optional(),
  })
  .refine((f) => (f.sql_url === undefined) !== (f.sql_inline === undefined), 'exactly one of sql_url or sql_inline')
  .refine((f) => f.sha256 === undefined || f.sql_url !== undefined, 'sha256 goes with sql_url');
export type SqlFixup = z.infer<typeof sqlFixup>;

export const benchStep = z.object({
  /** A dl-agent sub-command, e.g. `einvoice-enroll`. */
  setup: z
    .string()
    .regex(/^[a-z][a-z0-9-]{0,63}$/, 'a dl-agent sub-command')
    .refine((s) => !RESERVED.has(s), 'reserved: the bench script runs it itself'),
  /** Plain arguments (visible in the process list): no secrets here, use `env`. May hold `{{BENCH_DIR}}`. */
  args: z.array(plain(500)).max(50).default([]),
  /** Set for this step's process only; values may hold `{{SQL:<DbName>}}`, `{{SQL_SERVER}}` (etc.) and `{{BENCH_DIR}}`. */
  env: z
    .record(z.string().regex(/^[A-Za-z_][A-Za-z0-9_]{0,63}$/, 'an environment variable name'), plain(8000))
    .default({})
    .refine((env) => Object.keys(env).length <= 20, 'at most 20 variables'),
});
export type BenchStep = z.infer<typeof benchStep>;

export const benchBody = z
  .object({
    tenant_id: id,
    site_id: id,
    label: plain(200).min(1),
    steps: z.array(benchStep).min(1).max(30),
    sql_databases: z.array(sqlName).max(20).default([]),
    sql_fixups: z.array(sqlFixup).max(20).default([]),
    expires_hours: z.number().int().min(1).max(72).default(24),
    max_fetches: z.number().int().min(1).max(100).default(10),
    /**
     * Remove the older per-product agents before installing: scheduled tasks unregistered, services
     * stopped and deleted, folders renamed to `<dir>.bench-old-<time>`. Bundles made before this
     * field only list them.
     */
    remove_legacy: z.boolean().default(true),
    /**
     * With Windows authentication, give `NT AUTHORITY\\SYSTEM` (the agent service account) a login,
     * a user and `db_owner` in each of `sql_databases`, so the service can open them. For test
     * copies only; off by default.
     */
    sql_grant_service: z.boolean().default(false),
    /** Initial remote config of the enrolled agent, e.g. `{"modules":{"einvoice":{"enabled":true}}}`. */
    config: z.record(z.string(), z.unknown()).optional(),
  })
  .superRefine((b, ctx) => {
    b.steps.forEach((s, i) => {
      s.args.forEach((arg, j) => {
        for (const m of arg.matchAll(ANY_PLACEHOLDER)) {
          if (m[1] !== BENCH_DIR) {
            ctx.addIssue({ code: 'custom', path: ['steps', i, 'args', j], message: `only {{BENCH_DIR}} in args (SQL connections and their parts go in env): ${m[0]}` });
          }
        }
      });
      for (const [name, value] of Object.entries(s.env)) {
        for (const m of value.matchAll(ANY_PLACEHOLDER)) {
          const inner = m[1] ?? '';
          const ok = inner === BENCH_DIR || SQL_PART.has(inner) || (inner.startsWith('SQL:') && sqlName.safeParse(inner.slice(4)).success);
          if (!ok) ctx.addIssue({ code: 'custom', path: ['steps', i, 'env', name], message: `unknown placeholder or bad database name: ${m[0]}` });
        }
      }
    });
  });
export type BenchBody = z.infer<typeof benchBody>;

/** What is sealed in `steps_enc`: everything the script needs that must not sit in D1 in clear. */
export interface BenchPayload {
  code: string;
  steps: BenchStep[];
  sql_databases: string[];
  sql_fixups?: SqlFixup[];
  /** Absent in bundles sealed before it existed: those only list the older agents. */
  remove_legacy?: boolean;
  /** Absent in bundles sealed before it existed: nothing is granted. */
  sql_grant_service?: boolean;
}

/** Every database a bundle touches: registered, fixed up, or named by an env placeholder. */
export function sqlNamesOf(p: Pick<BenchPayload, 'steps' | 'sql_databases' | 'sql_fixups'>): string[] {
  const names = new Set(p.sql_databases);
  for (const f of p.sql_fixups ?? []) names.add(f.database);
  for (const s of p.steps) {
    for (const value of Object.values(s.env)) {
      for (const m of value.matchAll(SQL_PLACEHOLDER)) if (m[1]) names.add(m[1]);
    }
  }
  return [...names];
}

/** True when a step takes the SQL connection field by field (`{{SQL_SERVER}}` …). */
export function usesSqlParts(steps: readonly BenchStep[]): boolean {
  return steps.some((s) => Object.values(s.env).some((v) => SQL_PARTS.some((p) => v.includes(`{{${p}}}`))));
}
