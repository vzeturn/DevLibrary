import type { Channel, Env } from '../env';
import { manifest as manifestSchema } from '../domain/schemas';
import { HttpError, parseJson, validate } from '../lib/http';
import { getRelease, latestRelease } from '../repo/releases';

/** What the installer needs to fetch and check one build. */
export interface InstallInfo {
  product: string;
  channel: Channel;
  version: string;
  target: string;
  sha256: string;
  size: number;
  /** The hub's clock (Unix seconds), so the installer can refuse a machine the hub would reject. */
  server_time: number;
  /** Public download path on this hub. */
  download: string;
}

const ARTIFACT_PATH = '/v1/artifacts/';

function artifactOf(text: string, target: string): { url: string; sha256: string; size: number; version: string } {
  const m = validate(manifestSchema, parseJson(text));
  const a = m.artifacts.find((x) => x.target === target);
  if (!a) throw new HttpError(404, 'no_artifact', { target, available: m.artifacts.map((x) => x.target) });
  return { url: a.url, sha256: a.sha256, size: a.size, version: m.version };
}

/** The build of the latest release on `channel` for `target`. */
export async function installInfo(env: Env, product: string, channel: Channel, target: string, now: number): Promise<InstallInfo> {
  const row = await latestRelease(env.DB, product, channel);
  if (!row) throw new HttpError(404, 'no_release');
  const a = artifactOf(row.manifest, target);
  return {
    product,
    channel,
    version: a.version,
    target,
    sha256: a.sha256,
    size: a.size,
    server_time: now,
    download: `/install/download/${encodeURIComponent(product)}/${channel}/${encodeURIComponent(a.version)}/${encodeURIComponent(target)}`,
  };
}

/** `key=value` lines: PowerShell 2.0 (Windows 7) has no JSON parser. */
export function infoText(i: InstallInfo): string {
  return Object.entries(i)
    .map(([k, v]) => `${k}=${String(v)}`)
    .join('\n')
    .concat('\n');
}

export type Download = { kind: 'object'; object: R2ObjectBody } | { kind: 'redirect'; url: string };

/**
 * The published build of one release. Builds stored on this hub stream from R2 (the agent route
 * needs a signed agent, which a machine being installed is not yet); others redirect.
 */
export async function download(env: Env, origin: string, product: string, channel: Channel, version: string, target: string): Promise<Download> {
  const row = await getRelease(env.DB, product, version, channel);
  if (!row) throw new HttpError(404, 'release_not_found');
  const { url } = artifactOf(row.manifest, target);
  const parsed = new URL(url);
  if (parsed.origin !== origin || !parsed.pathname.startsWith(ARTIFACT_PATH)) return { kind: 'redirect', url };
  const key = decodeURIComponent(parsed.pathname.slice(ARTIFACT_PATH.length));
  const object = await env.ARTIFACTS.get(key);
  if (!object) throw new HttpError(404, 'artifact_not_found');
  return { kind: 'object', object };
}
