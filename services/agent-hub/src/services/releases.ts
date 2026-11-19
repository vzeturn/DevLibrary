import type { AgentRow, Channel, Env } from '../env';
import { evaluateGate, type GateResult } from '../domain/gate';
import { gateRules } from '../domain/lanes';
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
import { agentHeld, releasePause, updateNowActive } from './holds';
import { laneConfig, releaseLane, type LaneStatus } from './lanes';
import { requirementOf, siteReadiness } from './sites';

export interface GateStatus extends GateResult {
  product: string;
  version: string;
  previous_version: string | null;
  /** Set by the release's bump (PROTOCOL §5.2). */
  min_agents: number;
  soak_hours: number;
  lane: LaneStatus;
  /** Paused releases are never offered (PROTOCOL §5.3). */
  paused: boolean;
  paused_reason: string | null;
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
export async function manifestText(env: Env, product: string, channel: Channel, agent?: AgentRow, now = 0): Promise<string> {
  if (agent && (await agentHeld(env, agent, product)) && !(await updateNowActive(env, agent.id, now))) {
    throw new HttpError(404, 'no_release');
  }
  const row = await latestRelease(env.DB, product, channel);
  if (!row) throw new HttpError(404, 'no_release');
  return row.manifest;
}

export async function gateStatus(env: Env, product: string, version: string, now: number): Promise<GateStatus> {
  const canary = await getRelease(env.DB, product, version, 'canary');
  if (!canary) throw new HttpError(404, 'release_not_found');
  const lane = await releaseLane(env, product, version, now);
  const { minAgents, soakHours } = gateRules(laneConfig(env), lane.bump);
  const previous = await previousVersion(env.DB, product, version, canary.published_at);
  const [agents, errorFingerprints, previousFingerprints] = await Promise.all([
    gateAgents(env.DB, product, version),
    fingerprintsFor(env.DB, version, true),
    previous ? fingerprintsFor(env.DB, previous, false) : Promise.resolve([]),
  ]);
  const requirement = await requirementOf(env, product);
  const live = agents.filter((a) => a.on_version && !a.rolled_back).map((a) => a.agent_id);
  const unreadySites = requirement
    ? (await siteReadiness(env, requirement, { agentIds: live }, now))
        .filter((s) => !s.eligible)
        .map((s) => ({ agent_id: s.agent_id, reasons: s.reasons.map((r) => r.detail) }))
    : null;
  const pause = await releasePause(env, product, version);
  const result = evaluateGate({
    minAgents,
    soakHours,
    agents,
    errorFingerprints,
    previousVersion: previous,
    previousFingerprints,
    unreadySites,
  });
  return { product, version, previous_version: previous, min_agents: minAgents, soak_hours: soakHours, lane, paused: pause !== null, paused_reason: pause?.reason ?? null, ...result };
}

export interface PromoteAnswer {
  promoted: true;
  forced: boolean;
  gate: GateStatus;
}

export async function promote(env: Env, product: string, version: string, req: PromoteRequest, now: number): Promise<PromoteAnswer> {
  const stable = checkManifest(req.manifest, product, version, 'stable');
  const gate = await gateStatus(env, product, version, now);
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
      bump: gate.lane.bump,
      reason: req.reason?.trim() || null,
      checks: JSON.stringify(gate.checks),
      promoted_at: now,
    },
    { product, version, channel: 'stable', manifest: req.manifest, published_at: stable.published_at, created_at: now },
  );
  return { promoted: true, forced, gate };
}
