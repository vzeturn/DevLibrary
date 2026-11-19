import type { Env } from '../env';
import { psLiteral, selectDemoModules, type DemoBenchBody } from '../domain/demo';
import { HttpError } from '../lib/http';
import { getTenant, upsertTenant } from '../repo/tenants';
import { demoCommand } from '../install/script';
import { createEnrollCode } from './admin';

export interface DemoModuleView {
  module: string;
  product: string;
  setup: string | null;
  /**
   * Where the module's code comes from: `operator` (set on the machine; no product code is minted
   * by this hub, they all come from the product's own cloud) or `none` (nothing to enrol).
   */
  source: 'operator' | 'none';
  secrets: { env: string; required: boolean; what: string }[];
  options: { env: string; flag: string; required: boolean; what: string; value?: string }[];
  note: string;
}

export interface DemoBenchSet {
  tenant_id: string;
  site_id: string;
  channel: 'canary';
  enroll_code_id: string;
  /** One-time; shown only here (D1 keeps its SHA-256). */
  enroll_code: string;
  expires_at: number;
  install_command: string;
  modules: DemoModuleView[];
  /** PowerShell lines to paste before `install_command`: prefilled options, placeholders for the codes. */
  operator_env: string[];
}

/**
 * A demo code set for one machine: the demo tenant (created as a demo tenant, so its agents follow
 * canary; a live tenant is refused), a one-time dl-agent enrolment code whose first config enables
 * the selected modules, and what the operator must supply for each module. Nothing new is stored:
 * the code is an ordinary enrolment code, the module codes never reach the hub.
 */
export async function createDemoBench(env: Env, body: DemoBenchBody, origin: string, now: number): Promise<DemoBenchSet> {
  const tenant = await getTenant(env.DB, body.tenant_id);
  if (!tenant) {
    await upsertTenant(env.DB, { id: body.tenant_id, name: body.tenant_id, demo: true }, now);
  } else if (tenant.demo !== 1) {
    // A live tenant would see a test PC among its agents, on canary.
    throw new HttpError(409, 'tenant_not_demo');
  }
  const mods = selectDemoModules(body.modules);
  const config = { modules: Object.fromEntries(mods.map((m) => [m.module, { enabled: true }])) };
  const code = await createEnrollCode(
    env,
    { tenant_id: body.tenant_id, site_id: body.site_id, ttl_seconds: body.expires_hours * 3600, config },
    now,
  );

  const modules: DemoModuleView[] = mods.map((m) => ({
    module: m.module,
    product: m.product,
    setup: m.setup,
    source: m.setup === null ? 'none' : 'operator',
    secrets: m.secrets.map((x) => ({ env: x.from, required: x.required, what: x.what })),
    options: m.options.map((x) => {
      const value = body.settings[x.from];
      return { env: x.from, flag: x.flag, required: x.required, what: x.what, ...(value === undefined ? {} : { value }) };
    }),
    note: m.note,
  }));

  const operator_env: string[] = [];
  for (const m of modules) {
    for (const x of m.options) {
      if (x.value !== undefined) operator_env.push(`$env:${x.env} = ${psLiteral(x.value)}`);
      else if (x.required) operator_env.push(`$env:${x.env} = '<${x.what}>'`);
    }
    for (const x of m.secrets) {
      if (x.required) operator_env.push(`$env:${x.env} = '<${x.what}>'`);
    }
  }

  return {
    tenant_id: body.tenant_id,
    site_id: body.site_id,
    channel: 'canary',
    enroll_code_id: code.id,
    enroll_code: code.code,
    expires_at: code.expires_at,
    install_command: demoCommand(origin, code.code, body.modules),
    modules,
    operator_env,
  };
}
