import type { AgentRow, Env } from '../env';
import { sha256Hex, uuidv7 } from '../lib/crypto';
import { HttpError, logError } from '../lib/http';
import { getDiagnostics, insertDiagnostics } from '../repo/diagnostics';

/** Largest bundle the hub takes (PROTOCOL.md §4.1); checked before the signature is verified. */
export const MAX_DIAGNOSTICS_BYTES = 8 * 1024 * 1024;

/** R2 prefix of the bundles; the agent artifact route never serves it. */
export const DIAGNOSTICS_PREFIX = 'diag/';

const ZIP_MAGIC = [0x50, 0x4b, 0x03, 0x04];

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
 * Keeps one bundle: R2 object first, then the D1 row. If the row cannot be written the object
 * is deleted again (a failure to delete is logged) and the error goes to the caller.
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
  if (commandId !== null && !/^[A-Za-z0-9._-]{1,64}$/.test(commandId)) throw new HttpError(400, 'bad_command_id');
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
      command_id: commandId,
      agent_version: agent.version,
      created_at: now,
    });
  } catch (err) {
    await env.ARTIFACTS.delete(key).catch((e: unknown) => logError('diagnostics_orphan_delete_failed', e, { key }));
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
