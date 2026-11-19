/** Test helper: signs Cloudflare Access-style RS256 tokens. */
const b64url = (bytes: Uint8Array | string): string =>
  Buffer.from(typeof bytes === 'string' ? new TextEncoder().encode(bytes) : bytes).toString('base64url');

export interface Signer {
  jwk: JsonWebKey & { kid: string };
  sign(payload: Record<string, unknown>, header?: Record<string, unknown>): Promise<string>;
}

/** An RS256 signer with its public JWK, as Access publishes it. */
export async function accessSigner(kid = 'k1'): Promise<Signer> {
  const pair = (await crypto.subtle.generateKey(
    { name: 'RSASSA-PKCS1-v1_5', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' },
    true,
    ['sign', 'verify'],
  )) as CryptoKeyPair;
  const pub = (await crypto.subtle.exportKey('jwk', pair.publicKey)) as JsonWebKey;
  return {
    jwk: { kty: pub.kty, n: pub.n, e: pub.e, kid },
    async sign(payload, header = {}) {
      const h = b64url(JSON.stringify({ alg: 'RS256', kid, typ: 'JWT', ...header }));
      const p = b64url(JSON.stringify(payload));
      const sig = await crypto.subtle.sign('RSASSA-PKCS1-v1_5', pair.privateKey, new TextEncoder().encode(`${h}.${p}`));
      return `${h}.${p}.${b64url(new Uint8Array(sig))}`;
    },
  };
}
