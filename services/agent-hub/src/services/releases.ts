import { intVar, type Channel, type Env } from '../env';
import { evaluateGate, type GateResult } from '../domain/gate';
import { manifest as manifestSchema, type Manifest } from '../domain/schemas';
import { uuidv7 } from '../lib/crypto';
import { HttpError, parseJson, validate } from '../lib/http';
import {
  fingerprintsFor,
  gateAgents,
  getRelease,
  insertPromotion,
  latestRelease,
  previousVersion,
  putRelease,
} from '../repo/releases';

export interface GateStatus extends GateResult {
  product: string;
  version: string;
  previous_version: string | null;
  min_agents: number;
  soak_hours: number;
}

export interface PromoteRequest {
  manifest: string;
  force?: boolean;
  reason?: string;
}

/** Parses and shape-checks a manifest, requiring it to name this product, version and channel. */
export function checkManifest(text: string, product: string, version: string, channel: Channel): Manifest {
  const m = validate(manifestSchema, parseJson(text));
  if (m.product !== product || m.version !== version) {
    throw new HttpError(400, 'manifest_mismatch', { expected: { product, version }, got: { product: m.product, version: m.version } });
  }
  if (m.channel !== channel) throw new HttpError(400, 'manifest_channel', { expected: channel, got: m.channel });
  return m;
}

export async function publishCanary(env: Env, product: string, version: string, text: string, now: number): Promise<Manifest> {
  const m = checkManifest(text, product, version, 'canary');
  await putRelease(env.DB, { product, version, channel: 'canary', manifest: text, published_at: m.published_at, created_at: now });
  return m;
}

/** The manifest text exactly as published; the signature covers the agent's canonical form of it. */
export async function manifestText(env: Env, product: string, channel: Channel): Promise<string> {
  const row = await latestRelease(env.DB, product, channel);
  if (!row) throw new HttpError(404, 'no_release');
  return row.manifest;
}

export async function gateStatus(env: Env, product: string, version: string): Promise<GateStatus> {
  const canary = await getRelease(env.DB, product, version, 'canary');
  if (!canary) throw new HttpError(404, 'release_not_found');
  const minAgents = intVar(env.GATE_MIN_AGENTS, 1);
  const soakHours = intVar(env.GATE_SOAK_HOURS, 2);
  const previous = await previousVersion(env.DB, product, version, canary.published_at);
  const [agents, errorFingerprints, previousFingerprints] = await Promise.all([
    gateAgents(env.DB, product, version),
    fingerprintsFor(env.DB, version, true),
    previous ? fingerprintsFor(env.DB, previous, false) : Promise.resolve([]),
  ]);
  const result = evaluateGate({ minAgents, soakHours, agents, errorFingerprints, previousVersion: previous, previousFingerprints });
  return { product, version, previous_version: previous, min_agents: minAgents, soak_hours: soakHours, ...result };
}

export interface PromoteAnswer {
  promoted: true;
  forced: boolean;
  gate: GateStatus;
}

export async function promote(env: Env, product: string, version: string, req: PromoteRequest, now: number): Promise<PromoteAnswer> {
  const stable = checkManifest(req.manifest, product, version, 'stable');
  const gate = await gateStatus(env, product, version);
  const forced = !gate.pass;
  if (forced) {
    if (!req.force) throw new HttpError(409, 'gate_failed', { checks: gate.checks.filter((c) => !c.ok) });
    if (!req.reason?.trim()) throw new HttpError(400, 'force_requires_reason');
  }
  await insertPromotion(
    env.DB,
    {
      id: uuidv7(now * 1000),
      product,
      version,
      forced,
      reason: req.reason?.trim() || null,
      checks: JSON.stringify(gate.checks),
      promoted_at: now,
    },
    { product, version, channel: 'stable', manifest: req.manifest, published_at: stable.published_at, created_at: now },
  );
  return { promoted: true, forced, gate };
}
