import { beforeEach, describe, expect, it } from 'vitest';
import { accessConfig, clearAccessKeyCache, verifyAccessJwt, type JwksFetcher } from '../src/lib/accessJwt';
import { accessSigner, type Signer } from './accessSigner';

const TEAM = 'acme-ops.cloudflareaccess.com';
const AUD = 'aud-tag-123';
const NOW = 1_800_000_000;
const cfg = { teamDomain: TEAM, aud: AUD };

const claims = (extra: Record<string, unknown> = {}): Record<string, unknown> => ({
  aud: [AUD],
  iss: `https://${TEAM}`,
  exp: NOW + 300,
  iat: NOW - 10,
  email: 'ops@example.test',
  ...extra,
});

let signer: Signer;
let fetched: string[];
let fetcher: JwksFetcher;

beforeEach(async () => {
  clearAccessKeyCache();
  signer = await accessSigner();
  fetched = [];
  fetcher = async (url) => {
    fetched.push(url);
    return { keys: [signer.jwk] };
  };
});

describe('Access JWT', () => {
  it('names the user of a valid token and caches the team keys', async () => {
    expect(await verifyAccessJwt(await signer.sign(claims()), cfg, NOW, fetcher)).toEqual({ actor: 'ops@example.test' });
    expect(await verifyAccessJwt(await signer.sign(claims()), cfg, NOW, fetcher)).toEqual({ actor: 'ops@example.test' });
    expect(fetched).toEqual([`https://${TEAM}/cdn-cgi/access/certs`]);
  });

  it('names a service token by its client id', async () => {
    const token = await signer.sign(claims({ email: undefined, common_name: 'abc.access' }));
    expect(await verifyAccessJwt(token, cfg, NOW, fetcher)).toEqual({ actor: 'service-token:abc.access' });
  });

  it('trusts nothing that is missing, forged, foreign or out of date', async () => {
    const other = await accessSigner('k1');
    const cases: Array<string | undefined> = [
      undefined,
      'not-a-jwt',
      'a.b.c',
      await other.sign(claims()), // same kid, another key
      await signer.sign(claims({ aud: ['someone-else'] })),
      await signer.sign(claims({ iss: 'https://evil.cloudflareaccess.com' })),
      await signer.sign(claims({ exp: NOW - 120 })),
      await signer.sign(claims({ nbf: NOW + 600 })),
      await signer.sign(claims({ exp: undefined })),
      await signer.sign(claims({ email: undefined })),
      await signer.sign(claims(), { alg: 'none' }),
      await signer.sign(claims(), { kid: 'unknown' }),
    ];
    for (const token of cases) expect(await verifyAccessJwt(token, cfg, NOW, fetcher)).toBeNull();
  });

  it('looks the keys up again once for a rotated key', async () => {
    await verifyAccessJwt(await signer.sign(claims()), cfg, NOW, fetcher);
    const rotated = await accessSigner('k2');
    fetcher = async (url) => {
      fetched.push(url);
      return { keys: [signer.jwk, rotated.jwk] };
    };
    expect(await verifyAccessJwt(await rotated.sign(claims()), cfg, NOW, fetcher)).toEqual({ actor: 'ops@example.test' });
    expect(fetched).toHaveLength(2);
  });

  it('is configured only by both the team domain and the audience', () => {
    expect(accessConfig({})).toBeNull();
    expect(accessConfig({ ACCESS_TEAM_DOMAIN: TEAM })).toBeNull();
    expect(accessConfig({ ACCESS_TEAM_DOMAIN: `https://${TEAM}/`, ACCESS_AUD: AUD })).toEqual(cfg);
  });
});
