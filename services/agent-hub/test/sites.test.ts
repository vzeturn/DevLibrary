import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { evaluateGate } from '../src/domain/gate';
import { checkSite, siteRequirement, SURVEY_STALE_SECONDS, type SiteRequirement } from '../src/domain/site';
import { admin, enrollAgent, heartbeatBody, manifestText, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

const H = 3600;

/** SQL Server 2008 R2 on Windows 7, owner login, schema probed clean. */
function survey(db: Record<string, unknown> = {}, host: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    host: { family: 'windows', target: 'windows7-x86', nt_version: '6.1.7601', legacy_windows: true, ...host },
    databases: {
      SMILE_FO: {
        engine: { major: 10, name: 'SQL Server 2008 R2' },
        database: { name: 'SMILE_FO', compatibility_level: 100, collation: 'Vietnamese_CI_AS' },
        grants: { sysadmin: false, db_owner: false, execute: true, create_procedure: true, alter_dbo: true },
        warnings: ['engine_pre_2012', 'express_edition'],
        probe: { ok: true, checked: 12, missing: [] },
        ...db,
      },
    },
  };
}
const req = (r: unknown): SiteRequirement => siteRequirement.parse(r);
const einvoice = req({
  host: { min_nt_version: '6.1', families: ['windows'] },
  databases: { SMILE_FO: { min_engine_major: 10, min_compatibility: 100, grants: ['execute', 'create_procedure'], forbid_warnings: ['database_read_only'], probe_ok: true } },
});
const codes = (s: unknown, r = einvoice, at: number | null = T0): string[] => checkSite(r, s, at, T0).reasons.map((x) => x.code);

describe('checkSite', () => {
  it('accepts a site that meets every requirement', () => {
    expect(checkSite(einvoice, survey(), T0, T0)).toEqual({ eligible: true, reasons: [] });
    expect(codes(survey(), req({}))).toEqual([]);
  });

  it('refuses a site with no survey or a stale one', () => {
    expect(codes(null, einvoice, null)).toEqual(['no_survey']);
    expect(codes(survey(), einvoice, T0 - SURVEY_STALE_SECONDS - 1)).toEqual(['survey_stale']);
  });

  it('checks the host family and Windows version', () => {
    expect(codes(survey({}, { nt_version: '6.0.6002' }))).toEqual(['windows_too_old']);
    expect(codes(survey({}, { nt_version: '10.0.19045' }))).toEqual([]);
    expect(codes(survey({}, { family: 'linux', nt_version: null }))).toEqual(['os_family']);
  });

  it('names each database shortfall', () => {
    expect(codes({ host: survey().host, databases: {} })).toEqual(['database_not_surveyed']);
    expect(codes(survey({ engine: { major: 9 } }))).toEqual(['engine_too_old']);
    expect(codes(survey({ database: { compatibility_level: 90 } }))).toEqual(['compatibility_too_low']);
    expect(codes(survey({ grants: { execute: true } }))).toEqual(['grant_missing']);
    expect(codes(survey({ grants: { db_owner: true } }))).toEqual([]);
    expect(codes(survey({ warnings: ['database_read_only'] }))).toEqual(['warning']);
  });

  it('requires a clean schema probe when asked', () => {
    expect(codes(survey({ probe: undefined }))).toEqual(['probe_absent']);
    const missing = survey({ probe: { ok: false, checked: 3, missing: [{ kind: 'column', name: 'dbo.HoaDon.MaCQT' }] } });
    const r = checkSite(einvoice, missing, T0, T0);
    expect(r.eligible).toBe(false);
    expect(r.reasons[0]?.detail).toBe('SMILE_FO: missing 1 (column dbo.HoaDon.MaCQT)');
  });

  it('feeds the site_eligible gate check', () => {
    const base = { minAgents: 0, soakHours: 0, agents: [], errorFingerprints: [], previousVersion: null, previousFingerprints: [] };
    const check = (unreadySites: { agent_id: string; reasons: string[] }[] | null) =>
      evaluateGate({ ...base, unreadySites }).checks.find((c) => c.name === 'site_eligible');
    expect(check(null)).toMatchObject({ ok: true, detail: 'no site requirements for this product' });
    expect(check([])?.ok).toBe(true);
    expect(check([{ agent_id: 'a1', reasons: ['SMILE_FO: compat 90 < 100'] }])).toMatchObject({ ok: false, detail: 'not ready: a1 (SMILE_FO: compat 90 < 100)' });
  });
});

describe('hub site readiness', () => {
  let env: TestEnv;
  let canary: TestAgent;
  const beat = async (version: string, extra: Record<string, unknown> = {}): Promise<void> => {
    expect((await signed(env, canary, 'POST', '/v1/heartbeat', heartbeatBody(canary, version, extra))).status).toBe(200);
  };

  beforeEach(async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    setNow(T0);
    env = testEnv();
    canary = await enrollAgent(env, 'demo', { demo: true });
  });
  afterEach(() => vi.useRealTimers());

  it('keeps the last survey across heartbeats that do not carry one', async () => {
    expect((await admin(env, 'GET', `/admin/agents/${canary.id}/survey`)).status).toBe(404);
    await beat('1.0.0', { survey: survey() });
    setNow(T0 + 60);
    await beat('1.0.0');
    const got = (await (await admin(env, 'GET', `/admin/agents/${canary.id}/survey`)).json()) as { received_at: number; survey: unknown };
    expect(got).toEqual({ received_at: T0, survey: survey() });
  });

  it('drops an oversized or malformed survey but still takes the heartbeat', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const big = survey({ probe: { ok: false, missing: Array.from({ length: 5000 }, (_, i) => ({ kind: 'column', name: `dbo.T.Column_${i}_${'x'.repeat(40)}` })) } });
    await beat('1.0.0', { survey: big });
    await beat('1.0.0', { survey: { host: 'win7' } });
    expect((await admin(env, 'GET', `/admin/agents/${canary.id}/survey`)).status).toBe(404);
    expect(warn.mock.calls.map((c) => JSON.parse(String(c[0])).reason)).toEqual([expect.stringMatching(/^too large/), 'host and databases must be objects']);
    warn.mockRestore();
  });

  it('stores requirements, lists ready sites and blocks the canary gate on an unready site', async () => {
    expect((await admin(env, 'GET', '/admin/products/dl-agent/sites')).status).toBe(404);
    expect((await admin(env, 'PUT', '/admin/products/dl-agent/site-requirements', { databases: { SMILE_FO: { grants: ['drop_everything'] } } })).status).toBe(400);
    const put = await admin(env, 'PUT', '/admin/products/dl-agent/site-requirements', { databases: { SMILE_FO: { min_compatibility: 100, probe_ok: true } } });
    expect(put.status).toBe(200);
    expect(await (await admin(env, 'GET', '/admin/products/dl-agent/site-requirements')).json()).toEqual({
      host: {},
      databases: { SMILE_FO: { min_compatibility: 100, grants: [], forbid_warnings: [], probe_ok: true } },
    });

    await admin(env, 'PUT', '/admin/releases/dl-agent/1.1.0', manifestText('1.1.0', 'canary', T0));
    await beat('1.1.0', { survey: survey({ database: { compatibility_level: 90 } }) });
    const sites = (await (await admin(env, 'GET', '/admin/products/dl-agent/sites')).json()) as { ready: number; sites: { agent_id: string; eligible: boolean }[] };
    expect(sites.ready).toBe(0);
    expect(sites.sites).toMatchObject([{ agent_id: canary.id, eligible: false }]);
    const gate = async (): Promise<{ name: string; ok: boolean }[]> =>
      ((await (await admin(env, 'GET', '/admin/releases/dl-agent/1.1.0/gate')).json()) as { checks: { name: string; ok: boolean }[] }).checks;
    expect((await gate()).find((c) => c.name === 'site_eligible')?.ok).toBe(false);

    setNow(T0 + 2 * H);
    await beat('1.1.0', { survey: survey() });
    expect((await gate()).every((c) => c.ok)).toBe(true);
    const page = await (await admin(env, 'GET', '/admin')).text();
    expect(page).toContain('Site sẵn sàng cho sản phẩm');
    expect(page).toContain('Sẵn sàng');

    expect((await admin(env, 'DELETE', '/admin/products/dl-agent/site-requirements')).status).toBe(200);
    expect((await admin(env, 'DELETE', '/admin/products/dl-agent/site-requirements')).status).toBe(404);
  });
});
