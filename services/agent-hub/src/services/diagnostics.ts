import type { AgentRow, Env } from '../env';
import { sha256Hex, uuidv7 } from '../lib/crypto';
import { HttpError, logError } from '../lib/http';
import { agentCommand } from '../repo/commands';
import { countDiagnosticsSince, diagnosticsForCommand, getDiagnostics, insertDiagnostics } from '../repo/diagnostics';

/** Largest bundle the hub takes (PROTOCOL.md §4.1); checked before the signature is verified. */
export const MAX_DIAGNOSTICS_BYTES = 8 * 1024 * 1024;

/** R2 prefix of the bundles; the agent artifact route never serves it. */
export const DIAGNOSTICS_PREFIX = 'diag/';

/** At most this many bundles per agent in {@link DIAGNOSTICS_WINDOW_SECONDS} (8 MB each). */
export const MAX_DIAGNOSTICS_PER_WINDOW = 4;
export const DIAGNOSTICS_WINDOW_SECONDS = 3600;

const ZIP_MAGIC = [0x50, 0x4b, 0x03, 0x04];
const COMMAND_ID = /^[A-Za-z0-9._-]{1,64}$/;

export interface StoredDiagnostics {
  id: string;
  key: string;
  size: number;
  sha256: string;
}

/** `diag/<tenant>/<agent>/<ts>.zip`, ts = `<unix ms>-<8 hex of the row id>`: two uploads never share a key. */
export function diagnosticsKey(agent: AgentRow, nowMs: number, id: string): string {
  return `${DIAGNOSTICS_PREFIX}${agent.tenant_id}/${agent.id}/${nowMs}-${id.replace(/-/g, '').slice(-8)}.zip`;
}

/**
 * Refuses an upload no open `collect_diagnostics` command asked for: the command must be this
 * agent's, of that kind, handed out and not yet answered, with no bundle stored for it yet; and
 * the agent must be under {@link MAX_DIAGNOSTICS_PER_WINDOW}.
 */
async function requireOpenRequest(env: Env, agent: AgentRow, commandId: string | null, now: number): Promise<string> {
  if (commandId === null || commandId === '') throw new HttpError(400, 'command_id_required');
  if (!COMMAND_ID.test(commandId)) throw new HttpError(400, 'bad_command_id');
  const cmd = await agentCommand(env.DB, agent.id, commandId);
  if (!cmd || cmd.kind !== 'collect_diagnostics' || cmd.delivered_at === null || cmd.result_at !== null) {
    throw new HttpError(409, 'no_open_diagnostics_command');
  }
  if (await diagnosticsForCommand(env.DB, commandId)) throw new HttpError(409, 'diagnostics_already_uploaded');
  const recent = await countDiagnosticsSince(env.DB, agent.id, now - DIAGNOSTICS_WINDOW_SECONDS + 1);
  if (recent >= MAX_DIAGNOSTICS_PER_WINDOW) {
    throw new HttpError(429, 'diagnostics_rate_limited', { max: MAX_DIAGNOSTICS_PER_WINDOW, window_seconds: DIAGNOSTICS_WINDOW_SECONDS });
  }
  return commandId;
}

/**
 * Keeps one bundle, uploaded for an open `collect_diagnostics` command ({@link requireOpenRequest}):
 * R2 object first, then the D1 row. If the row cannot be written the object is deleted again
 * (a failure to delete is logged) and the error goes to the caller; a second upload racing
 * for the same command loses on the unique index with 409.
 */
export async function storeDiagnostics(
  env: Env,
  agent: AgentRow,
  bytes: Uint8Array,
  commandId: string | null,
  now: number,
): Promise<StoredDiagnostics> {
  if (bytes.byteLength > MAX_DIAGNOSTICS_BYTES) throw new HttpError(413, 'diagnostics_too_large', { max_bytes: MAX_DIAGNOSTICS_BYTES });
  if (bytes.byteLength < ZIP_MAGIC.length || ZIP_MAGIC.some((b, i) => bytes[i] !== b)) throw new HttpError(400, 'not_a_zip');
  const command = await requireOpenRequest(env, agent, commandId, now);
  const id = uuidv7(now * 1000);
  const key = diagnosticsKey(agent, Date.now(), id);
  const sha256 = await sha256Hex(bytes);
  await env.ARTIFACTS.put(key, bytes, {
    httpMetadata: { contentType: 'application/zip' },
    customMetadata: { agent_id: agent.id, tenant_id: agent.tenant_id, sha256 },
  });
  try {
    await insertDiagnostics(env.DB, {
      id,
      agent_id: agent.id,
      tenant_id: agent.tenant_id,
      site_id: agent.site_id,
      r2_key: key,
      size: bytes.byteLength,
      sha256,
      command_id: command,
      agent_version: agent.version,
      created_at: now,
    });
  } catch (err) {
    await env.ARTIFACTS.delete(key).catch((e: unknown) => logError('diagnostics_orphan_delete_failed', e, { key }));
    if (err instanceof Error && /UNIQUE constraint failed: diagnostics\.command_id/.test(err.message)) {
      throw new HttpError(409, 'diagnostics_already_uploaded');
    }
    throw err;
  }
  return { id, key, size: bytes.byteLength, sha256 };
}

/** The stored zip of one bundle, for the admin download. */
export async function diagnosticsObject(env: Env, id: string): Promise<{ name: string; object: R2ObjectBody }> {
  const row = await getDiagnostics(env.DB, id);
  if (!row) throw new HttpError(404, 'diagnostics_not_found');
  const object = await env.ARTIFACTS.get(row.r2_key);
  if (!object) throw new HttpError(404, 'diagnostics_object_missing');
  return { name: `diag-${row.tenant_id}-${row.agent_id}-${row.created_at}.zip`, object };
}
