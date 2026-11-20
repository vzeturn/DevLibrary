import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { admin, enrollAgent, heartbeatBody, setNow, signed, T0, testEnv, type TestAgent, type TestEnv } from './helpers';

let env: TestEnv;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  setNow(T0);
  env = testEnv();
});
afterEach(() => vi.useRealTimers());

const PMS = {
  module: 'pms-site',
  secret: { DL_AGENT_PMS_CODE: 'PMS-ONE-TIME-7731' },
  options: { '--cloud-url': 'https://bench-pms.example' },
  external_id: 'azolo-demo',
};

const PDA = {
  module: 'pda-sync',
  secret: { DL_AGENT_PDA_CODE: 'PDA-ONE-TIME-4410' },
  options: { '--url': 'https://azolo-demo-pos.example' },
};

interface Wire {
  id: string;
  kind: string;
  module?: string;
  args: Record<string, unknown>;
}

async function heartbeat(agent: TestAgent, configVersion = 0): Promise<{ commands: Wire[]; config?: { version: number; body: Record<string, unknown> } }> {
  const res = await signed(env, agent, 'POST', '/v1/heartbeat', heartbeatBody(agent, '1.3.16', { config_version: configVersion }));
  expect(res.status).toBe(200);
  return (await res.json()) as { commands: Wire[]; config?: { version: number; body: Record<string, unknown> } };
}

function secrets(): unknown[] {
  return (env.DB.raw.prepare('SELECT secret FROM commands WHERE secret IS NOT NULL').all() as { secret: string }[]).map((r) => r.secret);
}

describe('POST /admin/agents/:id/attach', () => {
  it('needs the admin token', async () => {
    const agent = await enrollAgent(env, 'dl-bench');
    const res = await signed(env, agent, 'POST', `/admin/agents/${agent.id}/attach`, PMS);
    expect(res.status).toBe(401);
  });

  it('enables the module, queues the enrol tool with the code only in env, then a module restart', async () => {
    const agent = await enrollAgent(env, 'dl-bench');
    const res = await admin(env, 'POST', `/admin/agents/${agent.id}/attach`, PMS);
    expect(res.status).toBe(202);
    const answer = (await res.json()) as { config_version: number; command: { id: string; tool: string } };
    expect(answer).toMatchObject({ agent_id: agent.id, module: 'pms-site', command: { tool: 'pms-enroll' } });

    // The admin views never show the code, the audit row neither.
    const listed = await (await admin(env, 'GET', `/admin/commands?agent_id=${agent.id}`)).text();
    expect(listed).not.toContain('PMS-ONE-TIME-7731');
    expect(listed).toContain('"has_secret":true');
    const audit = JSON.stringify(env.DB.raw.prepare('SELECT * FROM admin_audit').all());
    expect(audit).toContain('/attach');
    expect(audit).not.toContain('PMS-ONE-TIME-7731');

    // The agent gets the config and the command, with the code under args.env, exactly once.
    const hb = await heartbeat(agent);
    expect(hb.config?.version).toBe(answer.config_version);
    expect((hb.config?.body.modules as Record<string, unknown>)['pms-site']).toEqual({ enabled: true });
    expect(hb.commands).toEqual([
      expect.objectContaining({
        id: answer.command.id,
        kind: 'attach',
        args: { module: 'pms-site', tool: 'pms-enroll', argv: ['--cloud-url', 'https://bench-pms.example'], env: { DL_AGENT_PMS_CODE: 'PMS-ONE-TIME-7731' } },
      }),
    ]);
    expect(hb.commands[0]?.module).toBeUndefined();
    expect(secrets()).toEqual([]);
    expect((await heartbeat(agent, answer.config_version)).commands).toEqual([]);

    // The tool succeeded: the hub queues restart_module for the module.
    const done = await signed(env, agent, 'POST', `/v1/commands/${answer.command.id}/result`, { ok: true, output: 'enrolled', finished_at: T0 });
    expect(done.status).toBe(200);
    const next = await heartbeat(agent, answer.config_version);
    expect(next.commands).toEqual([expect.objectContaining({ kind: 'restart_module', module: 'pms-site' })]);

    // The product link is recorded under the agent's own tenant.
    const view = (await (await admin(env, 'GET', `/admin/agents/${agent.id}`)).json()) as { agent: { products: { product: string; external_id: string }[] } };
    expect(view.agent.products).toEqual([expect.objectContaining({ product: 'pms-site', external_id: 'azolo-demo' })]);
  });

  it('queues no restart when the tool failed', async () => {
    const agent = await enrollAgent(env, 'dl-bench');
    const { command } = (await (await admin(env, 'POST', `/admin/agents/${agent.id}/attach`, PMS)).json()) as { command: { id: string } };
    await heartbeat(agent);
    await signed(env, agent, 'POST', `/v1/commands/${command.id}/result`, { ok: false, output: 'code expired', finished_at: T0 });
    expect((await heartbeat(agent, 99)).commands).toEqual([]);
  });

  it('keeps other modules and keys and does not bump the version when nothing changes', async () => {
    const agent = await enrollAgent(env, 'dl-bench');
    await admin(env, 'PUT', `/admin/agents/${agent.id}/config`, { body: { modules: { arrival: { enabled: true, poll: 5 } }, log_level: 'debug' } });
    const first = (await (await admin(env, 'POST', `/admin/agents/${agent.id}/attach`, { module: 'pms-device' })).json()) as Record<string, unknown>;
    expect(first).toMatchObject({ module: 'pms-device', command: null });
    const again = (await (await admin(env, 'POST', `/admin/agents/${agent.id}/attach`, { module: 'pms-device' })).json()) as Record<string, unknown>;
    expect(again.config_version).toBe(first.config_version);
    const row = env.DB.raw.prepare('SELECT body FROM configs WHERE agent_id = ?').get(agent.id) as { body: string };
    expect(JSON.parse(row.body)).toEqual({ modules: { arrival: { enabled: true, poll: 5 }, 'pms-device': { enabled: true } }, log_level: 'debug' });
    expect(env.DB.raw.prepare('SELECT COUNT(*) AS n FROM commands').get()).toEqual({ n: 0 });
  });

  it.each([
    [{ module: 'einvoice', secret: { DL_AGENT_EINVOICE_TOKEN: 'EI-TOKEN-9921' }, options: { '--platform-url': 'https://ei.example' }, reenroll: true }, 'einvoice-enroll', ['--platform-url', 'https://ei.example', '--force']],
    [{ module: 'arrival', secret: { DL_AGENT_ARRIVAL_TOKEN: 'AR-TOKEN-5512' }, options: { '--cloud-url': 'https://ar.example' }, reenroll: true }, 'arrival-enroll', ['--cloud-url', 'https://ar.example', '--force']],
    [{ ...PMS, flags: ['--force'] }, 'pms-enroll', ['--cloud-url', 'https://bench-pms.example', '--force']],
    [{ ...PDA, flags: ['--sql-admin-saved'] }, 'pda-enroll', ['--url', 'https://azolo-demo-pos.example', '--sql-admin-saved']],
    [
      { ...PDA, secret: { ...PDA.secret, DL_AGENT_PDA_SQL_ADMIN_PASSWORD: 'Adm1n-Pass' }, options: { ...PDA.options, '--sql-admin-user': 'sa' } },
      'pda-enroll',
      ['--url', 'https://azolo-demo-pos.example', '--sql-admin-user', 'sa'],
    ],
  ])('passes re-enrol and value-less flags to the tool (%#)', async (body, tool, argv) => {
    const agent = await enrollAgent(env, 'dl-bench');
    const res = await admin(env, 'POST', `/admin/agents/${agent.id}/attach`, body);
    expect(res.status).toBe(202);
    const hb = await heartbeat(agent);
    expect(hb.commands).toEqual([expect.objectContaining({ kind: 'attach', args: expect.objectContaining({ tool, argv, env: body.secret }) })]);
    const listed = await (await admin(env, 'GET', '/admin/commands')).text();
    for (const v of Object.values(body.secret)) expect(listed).not.toContain(v);
  });

  describe('the module directory from the agent config', () => {
    const TOKENS = {
      einvoice: { DL_AGENT_EINVOICE_TOKEN: 'EI-TOKEN-9921' },
      arrival: { DL_AGENT_ARRIVAL_TOKEN: 'AR-TOKEN-5512' },
      membership: { DL_AGENT_MEMBERSHIP_TOKEN: 'MB-TOKEN-3307' },
    } as const;
    const URL_OPT = { einvoice: ['--platform-url', 'https://ei.example'], arrival: ['--cloud-url', 'https://ar.example'], membership: ['--hub-url', 'https://mb.example'] } as const;

    async function attachArgvFor(module: keyof typeof TOKENS, modules: Record<string, unknown>): Promise<string[]> {
      const agent = await enrollAgent(env, 'dl-bench');
      if (Object.keys(modules).length > 0) await admin(env, 'PUT', `/admin/agents/${agent.id}/config`, { body: { modules } });
      const [flag, val] = URL_OPT[module];
      const res = await admin(env, 'POST', `/admin/agents/${agent.id}/attach`, { module, secret: TOKENS[module], options: { [flag]: val } });
      expect(res.status).toBe(202);
      const cmd = (await heartbeat(agent)).commands[0];
      return (cmd?.args as { argv: string[] }).argv;
    }

    it.each([
      ['einvoice', 'config_dir', '--config-dir', 'C:\\ProgramData\\dl-agent\\bench\\einvoice'],
      ['arrival', 'install_dir', '--install-dir', 'C:\\ProgramData\\dl-agent\\bench\\arrival'],
      ['membership', 'program_data_dir', '--config-dir', 'C:\\ProgramData\\dl-agent\\bench\\membership'],
    ] as const)('%s: passes %s as %s', async (module, key, flag, dir) => {
      const argv = await attachArgvFor(module, { [module]: { enabled: true, [key]: `  ${dir} ` } });
      expect(argv).toEqual([...URL_OPT[module], flag, dir]);
    });

    it.each(['einvoice', 'arrival', 'membership'] as const)('%s: no flag when the key is unset', async (module) => {
      expect(await attachArgvFor(module, { [module]: { enabled: true } })).toEqual([...URL_OPT[module]]);
      expect(await attachArgvFor(module, {})).toEqual([...URL_OPT[module]]);
    });

    it('ignores an empty or multi-line value', async () => {
      expect(await attachArgvFor('einvoice', { einvoice: { config_dir: '   ' } })).toEqual([...URL_OPT.einvoice]);
      expect(await attachArgvFor('einvoice', { einvoice: { config_dir: 'C:\\x\n--force' } })).toEqual([...URL_OPT.einvoice]);
    });

    it('leaves a module without a directory key alone even if the config has one', async () => {
      const agent = await enrollAgent(env, 'dl-bench');
      await admin(env, 'PUT', `/admin/agents/${agent.id}/config`, { body: { modules: { 'pms-site': { enabled: true, config_dir: 'C:\\x', install_dir: 'C:\\y' } } } });
      expect((await admin(env, 'POST', `/admin/agents/${agent.id}/attach`, PMS)).status).toBe(202);
      expect((await heartbeat(agent)).commands[0]?.args).toMatchObject({ argv: ['--cloud-url', 'https://bench-pms.example'] });
    });
  });

  it('clears the secret of a command that expired before the agent came back', async () => {
    const agent = await enrollAgent(env, 'dl-bench');
    await admin(env, 'POST', `/admin/agents/${agent.id}/attach`, { ...PMS, ttl_seconds: 60 });
    expect(secrets()).toHaveLength(1);
    setNow(T0 + 61);
    const res = await admin(env, 'GET', '/admin/commands');
    expect(await res.text()).toContain('"status":"expired"');
    expect(secrets()).toEqual([]);
  });

  it.each([
    [{ ...PMS, module: 'nope' }, 400, 'unknown_module'],
    [{ ...PMS, secret: {} }, 400, 'missing_secret'],
    [{ ...PMS, secret: { ...PMS.secret, PATH: 'C:\\evil' } }, 400, 'unknown_secret'],
    [{ ...PMS, options: {} }, 400, 'missing_option'],
    [{ ...PMS, options: { ...PMS.options, '--data': 'C:\\x' } }, 400, 'unknown_option'],
    [{ ...PMS, options: { '--cloud-url': 'https://x\n--force' } }, 400, 'invalid_body'],
    [{ module: 'speaker', secret: { X: 'y' } }, 400, 'module_has_no_enrol_step'],
    [{ module: 'speaker', reenroll: true }, 400, 'module_has_no_enrol_step'],
    [{ ...PMS, flags: ['--sql-admin-saved'] }, 400, 'unknown_flag'],
    [{ ...PMS, flags: ['--data'] }, 400, 'unknown_flag'],
    [{ ...PDA, reenroll: true }, 400, 'reenroll_not_supported'],
  ])('refuses %j', async (body, status, error) => {
    const agent = await enrollAgent(env, 'dl-bench');
    const res = await admin(env, 'POST', `/admin/agents/${agent.id}/attach`, body);
    expect(res.status).toBe(status);
    expect(((await res.json()) as { error: string }).error).toBe(error);
    expect(env.DB.raw.prepare('SELECT COUNT(*) AS n FROM commands').get()).toEqual({ n: 0 });
  });

  it('refuses an unknown or revoked agent', async () => {
    expect((await admin(env, 'POST', '/admin/agents/nobody/attach', PMS)).status).toBe(404);
    const agent = await enrollAgent(env, 'dl-bench');
    expect((await admin(env, 'POST', `/admin/agents/${agent.id}/revoke`, {})).status).toBe(200);
    expect((await admin(env, 'POST', `/admin/agents/${agent.id}/attach`, PMS)).status).toBe(409);
  });
});
