import { z } from 'zod';

const VERSION = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;
export const versionString = z.string().regex(VERSION, 'expected x.y.z');
const name = z.string().min(1).max(100);
/** Largest site survey the hub stores (a few databases, each with its last schema probe). */
/** A product (module) name: also the `modules.<name>` key and the tenant id prefix of its key. */
export const productName = z.string().regex(/^[a-z][a-z0-9-]{1,31}$/, 'lowercase letters, digits and - only');

export const MAX_SURVEY_BYTES = 256 * 1024;

export const enrollBody = z.object({
  code: z.string().min(4).max(64),
  public_key: z.string().min(1).max(100),
  hostname: z.string().max(255),
  os: z.string().max(50),
  arch: z.string().max(50),
  version: versionString,
  modules: z.array(name).max(100),
});
export type EnrollBody = z.infer<typeof enrollBody>;

/** `POST /v1/attach`: another product's one-time code for an enrolled agent (RT-0298). */
export const attachBody = z.object({ code: z.string().min(4).max(64) });
export type AttachBody = z.infer<typeof attachBody>;

export const moduleState = z.enum(['running', 'starting', 'degraded', 'stopped', 'disabled']);

export const heartbeatBody = z.object({
  agent_id: z.string(),
  /** Release line the agent follows; product agents built on dl-agent send their own name. */
  product: name.default('dl-agent'),
  version: versionString,
  channel: z.string().optional(),
  target: z.string().max(100).optional(),
  hostname: z.string().max(255).optional(),
  started_at: z.number().int().optional(),
  uptime_seconds: z.number().int().optional(),
  config_version: z.number().int().min(0).default(0),
  update: z
    .object({ state: z.enum(['idle', 'probation', 'rolled_back']), version: z.string().max(64).optional() })
    .optional(),
  modules: z
    .array(
      z.object({
        name,
        state: moduleState,
        detail: z.string().max(2000).optional(),
        restarts: z.number().int().min(0).default(0),
        last_error: z.string().max(4000).optional(),
        metrics: z.record(z.string(), z.unknown()).optional(),
      }),
    )
    .max(100)
    .default([]),
  /**
   * Site survey (§9.5): sent when it changed and at least daily; the hub keeps the last one.
   * Loosely typed on purpose: a malformed or oversized survey is dropped, never the heartbeat.
   */
  survey: z.unknown().optional(),
});
export type HeartbeatBody = z.infer<typeof heartbeatBody>;

export const severity = z.enum(['info', 'warning', 'error', 'critical']);
export type Severity = z.infer<typeof severity>;

export const problemReport = z.object({
  fingerprint: z.string().regex(/^[0-9a-f]{1,64}$/),
  severity,
  module: z.string().max(100),
  code: z.string().max(100),
  message: z.string().max(4000),
  context: z.record(z.string(), z.unknown()).default({}),
  first_seen: z.number().int(),
  last_seen: z.number().int(),
  count: z.number().int().min(1).default(1),
  agent_version: versionString,
  os: z.string().max(50).optional(),
  hostname: z.string().max(255).optional(),
});
export type ProblemReport = z.infer<typeof problemReport>;

export const reportsBody = z.object({ reports: z.array(problemReport).max(500) });

export const commandResultBody = z.object({
  ok: z.boolean(),
  output: z.string().max(64_000).default(''),
  finished_at: z.number().int(),
});

/** Release manifest (PROTOCOL §5). Shape only: the hub cannot and does not verify the release signature. */
export const manifest = z.object({
  product: name,
  channel: z.enum(['canary', 'stable']),
  version: versionString,
  published_at: z.number().int().positive(),
  rollout_percent: z.number().int().min(0).max(100),
  min_version: versionString.nullable(),
  allow_downgrade: z.boolean(),
  /** Installs outside the site's update window. Signed; written only when true. */
  urgent: z.boolean().optional(),
  /** Installs even where the site set `update.auto: false`. Signed; written only when true. */
  required: z.boolean().optional(),
  notes: z.string().optional(),
  artifacts: z
    .array(
      z.object({
        target: z.string().min(1),
        url: z.string().url(),
        sha256: z.string().regex(/^[0-9a-f]{64}$/),
        size: z.number().int().positive(),
      }),
    )
    .min(1),
  key_id: z.string().min(1),
  signature: z.string().min(1),
});
export type Manifest = z.infer<typeof manifest>;

/** Largest self-test upload the hub accepts. */
export const MAX_SELFTEST_BYTES = 256 * 1024;

export const selftestStatus = z.enum(['pass', 'warn', 'fail', 'skip']);
export type SelftestStatus = z.infer<typeof selftestStatus>;

/** `dl-agent selftest` result (`POST /v1/agent/selftest`). */
export const selftestBody = z.object({
  run_id: z.string().regex(/^[A-Za-z0-9._:-]{1,64}$/),
  trigger: z.string().regex(/^[a-z][a-z0-9_-]{0,31}$/),
  started_at: z.number().int(),
  finished_at: z.number().int(),
  agent_version: versionString,
  os: z.string().max(50).optional(),
  hostname: z.string().max(255).optional(),
  checks: z
    .array(
      z.object({
        module: name,
        id: z.string().min(1).max(100),
        title: z.string().max(300),
        status: selftestStatus,
        detail: z.string().max(4000).default(''),
        remedy: z.string().max(2000).optional(),
        duration_ms: z.number().int().min(0),
      }),
    )
    .max(500),
});
export type SelftestBody = z.infer<typeof selftestBody>;
