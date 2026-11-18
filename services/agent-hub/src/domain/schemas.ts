import { z } from 'zod';

const VERSION = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;
export const versionString = z.string().regex(VERSION, 'expected x.y.z');
const name = z.string().min(1).max(100);

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
