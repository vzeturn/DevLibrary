/**
 * Hub hosts the agents may call, primary first (PROTOCOL §2.1). The hub hands the list out in
 * the enrol and heartbeat answers so every agent learns the fallback host without a reinstall.
 */

/** Most hosts an agent keeps; the agent applies the same cap. */
export const MAX_HUB_URLS = 5;

function isLoopback(host: string): boolean {
  return host === 'localhost' || host === '127.0.0.1' || host === '[::1]';
}

/** A base URL an agent may sign requests to: https (http only for loopback), no credentials, no query. */
export function validHubUrl(raw: string): string | null {
  let u: URL;
  try {
    u = new URL(raw.trim());
  } catch {
    return null;
  }
  if (u.username || u.password || u.search || u.hash) return null;
  if (u.protocol !== 'https:' && !(u.protocol === 'http:' && isLoopback(u.hostname))) return null;
  const path = u.pathname.replace(/\/+$/, '');
  return `${u.protocol}//${u.host}${path}`;
}

/** `HUB_URLS` ("https://a,https://b"): valid, deduplicated, primary first, at most MAX_HUB_URLS. */
export function hubUrls(value: string | undefined): string[] {
  const out: string[] = [];
  for (const part of (value ?? '').split(',')) {
    const url = part.trim() ? validHubUrl(part) : null;
    if (url && !out.includes(url) && out.length < MAX_HUB_URLS) out.push(url);
  }
  return out;
}

/** The primary host (first of `HUB_URLS`), or null when the list is not configured. */
export function primaryHost(value: string | undefined): string | null {
  const first = hubUrls(value)[0];
  return first ? new URL(first).host : null;
}

/** Paths a fallback host serves: the agent API, health, and the installer. Admin, bench and products stay on the primary. */
export function servedOnFallback(path: string): boolean {
  return path === '/healthz' || path === '/install.ps1' || path.startsWith('/v1/') || path.startsWith('/install/');
}
