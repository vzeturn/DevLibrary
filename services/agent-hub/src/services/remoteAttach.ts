import { z } from 'zod';
import type { AgentRow, Env } from '../env';
import { DEMO_MODULES, type DemoModule } from '../domain/demo';
import { uuidv7 } from '../lib/crypto';
import { HttpError } from '../lib/http';
import { getAgent } from '../repo/agents';
import { agentCommand, clearExpiredSecrets, insertCommand } from '../repo/commands';
import { getConfig, putConfig } from '../repo/configs';
import { upsertProductLink } from '../repo/productLinks';
import { mergeModules } from './attach';
import { DEFAULT_CONFIG } from './config';

/**
 * Remote attach (RT-0261): an admin picks an agent that is already installed and switches a
 * product's module on for it from the hub, without touching the machine. The hub
 *   1. enables `modules.<module>` in the agent's config (a new version, like `PUT …/config`),
 *   2. for a module with an enrol tool, queues one `attach` command that runs the tool on the
 *      machine (`dl-agent <tool> --data … <options>`) with the product's one-time code in the
 *      variable the tool already reads. The code rides in the command's `secret`, which no view
 *      shows and which is cleared when the agent takes the command (or when it expires),
 *   3. after the agent reports the tool succeeded, queues `restart_module` so the module starts
 *      on its new credentials.
 * Which tool, variables and options a module takes is the demo bench table (`DEMO_MODULES`),
 * the single source the install script uses too.
 */
export const ATTACH_KIND = 'attach';

const value = z.string().min(1).max(4096);
const option = z.string().min(1).max(2048).regex(/^[^\r\n]+$/, 'one line');
export const remoteAttachBody = z.object({
  module: z.string().min(1).max(64),
  /** The tool's variables (e.g. `DL_AGENT_PMS_CODE`): the product's one-time code and other secrets. */
  secret: z.record(z.string(), value).default({}),
  /** The tool's plain options by flag (e.g. `--cloud-url`). */
  options: z.record(z.string(), option).default({}),
  /** Value-less flags from the module's list (e.g. `--sql-admin-saved` for pda-sync). */
  flags: z.array(z.string().min(1).max(64)).max(8).default([]),
  /** Enrol again over an existing enrolment (moves the module to this product tenant). */
  reenroll: z.boolean().default(false),
  /** The product's tenant id, recorded in `product_links` so the product finds its agent. */
  external_id: z.string().regex(/^[A-Za-z0-9._-]{1,64}$/).optional(),
  ttl_seconds: z.number().int().min(60).max(86400).default(3600),
});
export type RemoteAttachBody = z.infer<typeof remoteAttachBody>;

export interface RemoteAttachAnswer {
  agent_id: string;
  module: string;
  /** The config version that enables the module. */
  config_version: number;
  /** The queued enrol command, null for a module with nothing to enrol. */
  command: { id: string; tool: string; expires_at: number } | null;
}

function recipe(module: string): DemoModule {
  const m = DEMO_MODULES.find((x) => x.module === module);
  if (!m) throw new HttpError(400, 'unknown_module', { modules: DEMO_MODULES.map((x) => x.module) });
  return m;
}

/** The tool's argv from the options and flags, checked against the module's table row. */
export function attachArgv(m: DemoModule, options: Record<string, string>, flags: readonly string[] = [], reenroll = false): string[] {
  const known = new Set(m.options.map((o) => o.flag));
  const extra = Object.keys(options).filter((f) => !known.has(f));
  if (extra.length > 0) throw new HttpError(400, 'unknown_option', { options: extra, allowed: [...known] });
  const missing = m.options.filter((o) => o.required && !options[o.flag]).map((o) => o.flag);
  if (missing.length > 0) throw new HttpError(400, 'missing_option', { options: missing });
  const allowed = new Set(m.flags.map((f) => f.flag));
  const badFlags = flags.filter((f) => !allowed.has(f));
  if (badFlags.length > 0) throw new HttpError(400, 'unknown_flag', { flags: badFlags, allowed: [...allowed] });
  if (reenroll && !m.reenroll) throw new HttpError(400, 'reenroll_not_supported', { module: m.module });
  const wanted = new Set(flags);
  if (reenroll && m.reenroll) wanted.add(m.reenroll);
  return [
    ...m.options.flatMap((o) => (options[o.flag] ? [o.flag, options[o.flag] as string] : [])),
    ...m.flags.filter((f) => wanted.has(f.flag)).map((f) => f.flag),
  ];
}

/**
 * The module's configured files and directories as tool flags, so the tool writes where the module reads.
 * A key that is missing, unset, empty or not one plain line (CR, LF or NUL) is skipped on its own: the
 * tool then keeps its default for that one.
 */
export function dirArgv(m: DemoModule, config: unknown): string[] {
  if (m.dirs.length === 0 || !config || typeof config !== 'object') return [];
  const mods = (config as { modules?: unknown }).modules;
  const mod = mods && typeof mods === 'object' ? (mods as Record<string, unknown>)[m.module] : undefined;
  if (!mod || typeof mod !== 'object') return [];
  return m.dirs.flatMap((d) => {
    const raw = (mod as Record<string, unknown>)[d.key];
    if (typeof raw !== 'string') return [];
    const dir = raw.trim();
    if (dir === '' || dir.length > 2048 || /[\r\n\0]/.test(dir)) return [];
    return [d.flag, dir];
  });
}

/** The secret variables, checked against the module's table row. */
export function attachSecret(m: DemoModule, secret: Record<string, string>): Record<string, string> {
  const known = new Set(m.secrets.map((s) => s.to));
  const extra = Object.keys(secret).filter((k) => !known.has(k));
  if (extra.length > 0) throw new HttpError(400, 'unknown_secret', { variables: extra, allowed: [...known] });
  const missing = m.secrets.filter((s) => s.required && !secret[s.to]).map((s) => s.to);
  if (missing.length > 0) throw new HttpError(400, 'missing_secret', { variables: missing });
  return secret;
}

async function liveAgent(env: Env, agentId: string): Promise<AgentRow> {
  const agent = await getAgent(env.DB, agentId);
  if (!agent) throw new HttpError(404, 'agent_not_found');
  if (agent.revoked_at !== null) throw new HttpError(409, 'agent_revoked');
  return agent;
}

export async function remoteAttach(env: Env, agentId: string, body: RemoteAttachBody, now: number): Promise<RemoteAttachAnswer> {
  const m = recipe(body.module);
  if (!m.setup && (Object.keys(body.secret).length > 0 || Object.keys(body.options).length > 0 || body.flags.length > 0 || body.reenroll)) {
    throw new HttpError(400, 'module_has_no_enrol_step');
  }
  const args = m.setup ? attachArgv(m, body.options, body.flags, body.reenroll) : [];
  const secret = m.setup ? attachSecret(m, body.secret) : {};
  const agent = await liveAgent(env, agentId);
  await clearExpiredSecrets(env.DB, now);

  const current = await getConfig(env.DB, agent.id);
  const base = current ? (JSON.parse(current.body) as unknown) : DEFAULT_CONFIG.body;
  const argv = [...args, ...dirArgv(m, base)];
  const merged = mergeModules(base, { [m.module]: { enabled: true } });
  const changed = JSON.stringify(merged) !== JSON.stringify(base);
  const version = changed || !current ? await putConfig(env.DB, agent.id, JSON.stringify(merged), now) : current.version;

  if (body.external_id) {
    await upsertProductLink(
      env.DB,
      { product: m.module, external_id: body.external_id, tenant_id: agent.tenant_id, agent_id: agent.id, site_id: agent.site_id },
      now,
      body.reenroll,
    );
  }
  if (!m.setup) return { agent_id: agent.id, module: m.module, config_version: version, command: null };

  const id = uuidv7(now * 1000);
  const expires_at = now + body.ttl_seconds;
  await insertCommand(env.DB, {
    id,
    agent_id: agent.id,
    kind: ATTACH_KIND,
    module: null,
    args: JSON.stringify({ module: m.module, tool: m.setup, argv }),
    created_at: now,
    expires_at,
    secret: JSON.stringify(secret),
  });
  return { agent_id: agent.id, module: m.module, config_version: version, command: { id, tool: m.setup, expires_at } };
}

/** After a successful `attach` command: restart the module so it runs on its new credentials. */
export async function afterAttach(env: Env, agent: AgentRow, commandId: string, now: number): Promise<void> {
  const cmd = await agentCommand(env.DB, agent.id, commandId);
  if (!cmd || cmd.kind !== ATTACH_KIND) return;
  const args = JSON.parse(cmd.args) as { module?: unknown };
  if (typeof args.module !== 'string') return;
  await insertCommand(env.DB, {
    id: uuidv7(now * 1000),
    agent_id: agent.id,
    kind: 'restart_module',
    module: args.module,
    args: JSON.stringify({ module: args.module }),
    created_at: now,
    expires_at: now + 3600,
  });
}
