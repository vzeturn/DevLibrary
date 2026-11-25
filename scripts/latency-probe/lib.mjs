// Latency probe helpers (RT-1161, RT-1232): log in to the azolo-demo-pms tenant that is wired to
// dl-agent at SR and time read-only GET routes end to end. Response bodies carry real guest data:
// they are never printed or stored, only status, size and timing.
import { createHmac } from 'node:crypto';

export const API = process.env.LATENCY_PROBE_API ?? 'https://azolo-demo-pms.pms.id.vn/api/v1';

/** RFC 6238 TOTP (SHA-1, 30 s, 6 digits) from a base32 seed. */
export function totp(base32, now = Date.now()) {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  let bits = '';
  for (const c of base32.replace(/=+$/, '').replace(/\s/g, '').toUpperCase()) {
    const v = alphabet.indexOf(c);
    if (v < 0) throw new Error('TOTP seed is not base32');
    bits += v.toString(2).padStart(5, '0');
  }
  const key = Buffer.from(bits.match(/.{8}/g).map((b) => parseInt(b, 2)));
  const ctr = Buffer.alloc(8);
  ctr.writeBigUInt64BE(BigInt(Math.floor(now / 30000)));
  const h = createHmac('sha1', key).update(ctr).digest();
  const o = h[19] & 15;
  return String((h.readUInt32BE(o) & 0x7fffffff) % 1e6).padStart(6, '0');
}

/** Nearest-rank percentile of an unsorted sample (p in 0..100). */
export function percentile(samples, p) {
  if (samples.length === 0) return null;
  const s = [...samples].sort((a, b) => a - b);
  const rank = Math.ceil((p / 100) * s.length);
  return s[Math.min(s.length, Math.max(1, rank)) - 1];
}

export function summarize(samples) {
  const r = (v) => (v === null ? null : Math.round(v * 10) / 10);
  return { n: samples.length, p50: r(percentile(samples, 50)), p95: r(percentile(samples, 95)), p99: r(percentile(samples, 99)) };
}

function need(name) {
  const v = process.env[name];
  if (!v) throw new Error(`missing env ${name} (DevLibrary cloud kit / ~/.config/rt-kits)`);
  return v;
}

async function post(path, body) {
  const r = await fetch(API + path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  const t = await r.text();
  if (!r.ok) throw new Error(`${path} answered ${r.status}`);
  return JSON.parse(t);
}

export async function foLogin() {
  const a = await post('/auth/login', { loginName: need('AZOLO_DEMO_VIRTUAL_CLERK_LOGIN'), password: need('AZOLO_DEMO_VIRTUAL_CLERK_PASSWORD') });
  if (!a.mfaRequired) return a.accessToken;
  const b = await post('/auth/mfa/verify', { mfaToken: a.mfaToken, code: totp(need('AZOLO_DEMO_VIRTUAL_CLERK_TOTP_SECRET')) });
  return b.accessToken;
}

export async function posLogin() {
  const r = await post('/pos/auth/login', { pin: need('AZOLO_DEMO_POS_CASHIER_PIN'), rvcNo: 1 });
  return r.accessToken ?? r.token;
}

/** One timed GET; the body is read (so transfer time counts) and dropped. */
export async function timed(path, token) {
  const t0 = performance.now();
  const r = await fetch(API + path, { headers: token ? { authorization: 'Bearer ' + token } : {} });
  const buf = await r.arrayBuffer();
  const ms = performance.now() - t0;
  let code;
  if (!r.ok) { try { const j = JSON.parse(Buffer.from(buf).toString()); code = j.reason ?? j.code; } catch { code = undefined; } }
  return { ms, status: r.status, bytes: buf.byteLength, code };
}
