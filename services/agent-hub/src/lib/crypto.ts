const encoder = new TextEncoder();

export function hex(bytes: ArrayBuffer | Uint8Array): string {
  const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let out = '';
  for (const b of view) out += b.toString(16).padStart(2, '0');
  return out;
}

export async function sha256Hex(data: string | Uint8Array | ArrayBuffer): Promise<string> {
  const bytes = typeof data === 'string' ? encoder.encode(data) : data;
  return hex(await crypto.subtle.digest('SHA-256', bytes));
}

export function fromBase64(text: string): Uint8Array | null {
  try {
    const bin = atob(text);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/** Compares two secrets in time independent of where they differ (hashes equalise length). */
export async function constantTimeEqual(a: string, b: string): Promise<boolean> {
  const [ha, hb] = await Promise.all([
    crypto.subtle.digest('SHA-256', encoder.encode(a)),
    crypto.subtle.digest('SHA-256', encoder.encode(b)),
  ]);
  const va = new Uint8Array(ha);
  const vb = new Uint8Array(hb);
  let diff = 0;
  for (let i = 0; i < va.length; i++) diff |= (va[i] ?? 0) ^ (vb[i] ?? 0);
  return diff === 0;
}

export async function importEd25519(publicKey: Uint8Array): Promise<CryptoKey | null> {
  if (publicKey.length !== 32) return null;
  try {
    return await crypto.subtle.importKey('raw', publicKey, { name: 'Ed25519' }, false, ['verify']);
  } catch {
    return null;
  }
}

export async function verifyEd25519(publicKeyB64: string, signatureB64: string, message: string): Promise<boolean> {
  const pk = fromBase64(publicKeyB64);
  const sig = fromBase64(signatureB64);
  if (!pk || !sig || sig.length !== 64) return false;
  const key = await importEd25519(pk);
  if (!key) return false;
  return crypto.subtle.verify({ name: 'Ed25519' }, key, sig, encoder.encode(message));
}

/** UUID v7 (RFC 9562): 48-bit ms timestamp, then random bits. */
export function uuidv7(nowMs: number = Date.now()): string {
  const b = crypto.getRandomValues(new Uint8Array(16));
  let ts = nowMs;
  for (let i = 5; i >= 0; i--) {
    b[i] = ts % 256;
    ts = Math.floor(ts / 256);
  }
  b[6] = ((b[6] ?? 0) & 0x0f) | 0x70;
  b[8] = ((b[8] ?? 0) & 0x3f) | 0x80;
  const h = hex(b);
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/** Human-typable one-time code: 4 groups of 4 from an unambiguous alphabet (~80 bits). */
export function randomCode(): string {
  const alphabet = 'ABCDEFGHJKMNPQRSTUVWXYZ23456789';
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  let out = '';
  bytes.forEach((byte, i) => {
    if (i > 0 && i % 4 === 0) out += '-';
    out += alphabet[byte % alphabet.length];
  });
  return out;
}
