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

export function toBase64(bytes: Uint8Array): string {
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}

/** URL-safe random token (base64url, no padding) carrying `bytes * 8` bits. */
export function randomToken(bytes = 32): string {
  return toBase64(crypto.getRandomValues(new Uint8Array(bytes))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

const SEALED_PREFIX = 'v1.';

/** Imports a base64 AES-256 key (32 bytes); null when the secret is missing or malformed. */
export async function importAesKey(secret: string | undefined): Promise<CryptoKey | null> {
  const raw = secret ? fromBase64(secret.trim()) : null;
  if (!raw || raw.length !== 32) return null;
  return crypto.subtle.importKey('raw', raw, { name: 'AES-GCM' }, false, ['encrypt', 'decrypt']);
}

/** AES-256-GCM with a random 96-bit IV; `aad` binds the ciphertext to its row. Output `v1.<iv>.<ct>`. */
export async function seal(key: CryptoKey, plain: string, aad: string): Promise<string> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = await crypto.subtle.encrypt({ name: 'AES-GCM', iv, additionalData: encoder.encode(aad) }, key, encoder.encode(plain));
  return `${SEALED_PREFIX}${toBase64(iv)}.${toBase64(new Uint8Array(ct))}`;
}

/** Reverses {@link seal}; throws when the text is malformed, the key differs or the AAD does not match. */
export async function unseal(key: CryptoKey, sealed: string, aad: string): Promise<string> {
  const parts = sealed.startsWith(SEALED_PREFIX) ? sealed.slice(SEALED_PREFIX.length).split('.') : [];
  const iv = parts.length === 2 ? fromBase64(parts[0] ?? '') : null;
  const ct = parts.length === 2 ? fromBase64(parts[1] ?? '') : null;
  if (!iv || iv.length !== 12 || !ct) throw new Error('sealed value is malformed');
  const plain = await crypto.subtle.decrypt({ name: 'AES-GCM', iv, additionalData: encoder.encode(aad) }, key, ct);
  return new TextDecoder().decode(plain);
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
