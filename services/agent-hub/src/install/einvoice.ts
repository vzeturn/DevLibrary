/**
 * E-Invoice (RT-0164, smile100): switching a site from the MM-EInvoice Go agent to dl-agent with the
 * `einvoice` module. Owner rule: the old agent is uninstalled and the site enrols afresh (new key, new
 * device); only the data carries over:
 *
 * - `einvoice-enroll` reads the Go agent's `config.enc` (same folder, same format) for the SQL
 *   connection and the collect window, and `--switch` makes it enrol again over that file instead of
 *   keeping it (it keeps only a `config.enc` this dl-agent sealed, so every later run is a no-op);
 * - bills the Go agent pushed stay marked `MMEI_Collected`; it marks a bill only after a successful
 *   push, so anything left in its outbox is collected again, and the platform deduplicates on
 *   (tenant, source_ref) and the payload hash.
 *
 * `install.ps1` runs the setup while the Go agent still runs and removes its service (`-Replace`)
 * only after the setup succeeded. The Go agent has no scheduled tasks.
 *
 * Everything here is quoted into PowerShell, so each value is checked against a strict charset first.
 */
import type { BenchStep } from '../domain/bench';

/**
 * `-Replace` patterns (`-like` against service name and display name) of the Go agent: `MM-EInvoice Agent`
 * (MM-EINVOICE cmd/agent/main.go) or `MMEI-Agent` shown as `MM-EInvoice Agent` (deploy/agent/install-service.ps1).
 */
export const EINVOICE_GO_SERVICES = ['*MM-EInvoice*', 'MMEI-Agent'] as const;

/** The one-time enrolment token of the E-Invoice platform (MM-EINVOICE internal/webapi/enroll.go tokenPattern). */
const TOKEN = /^[A-Za-z0-9_-]{1,128}$/;
/** A dl-agent enrol code. */
const CODE = /^[A-Za-z0-9-]{1,64}$/;
/** An https origin, optionally with a plain path: no quotes, spaces or PowerShell metacharacters. */
const BASE_URL = /^https:\/\/[A-Za-z0-9.-]{1,255}(:[0-9]{1,5})?(\/[A-Za-z0-9._~\/-]{0,200})?$/;

function check(value: string, re: RegExp, what: string): string {
  if (!re.test(value)) throw new Error(`einvoice: invalid ${what}`);
  return value;
}

/** Arguments of `einvoice-enroll` for a switch. The token travels in `DL_AGENT_EINVOICE_TOKEN`, never here. */
export function einvoiceSetupArgs(platformUrl: string, opts: { sqlPrompt?: boolean } = {}): string[] {
  const args = ['--platform-url', check(platformUrl.replace(/\/+$/, ''), BASE_URL, 'platform URL'), '--switch'];
  if (opts.sqlPrompt) args.push('--sql-prompt');
  return args;
}

export interface EinvoicePilot {
  /** The dl-agent hub serving `install.ps1`, e.g. `https://agent.pms.id.vn`. */
  hub: string;
  /** A dl-agent enrol code for the site. */
  code: string;
  /** The E-Invoice platform the module enrols with. */
  platformUrl: string;
  /** The platform's one-time enrolment token. */
  token: string;
  /** SQL Server on another machine: ask for and verify the connection before the token is spent. */
  sqlPrompt?: boolean;
  canary?: boolean;
}

/**
 * The PowerShell (2.0 safe: WebClient, no `irm`) run as Administrator on a customer PC that still runs the
 * Go agent: installs dl-agent, enrols the `einvoice` module with `--switch`, then removes the Go agent's
 * service. Re-running it is safe: dl-agent is kept and `--switch` keeps its own enrolment unspent.
 */
export function einvoicePilotScript(p: EinvoicePilot): string {
  const hub = check(p.hub.replace(/\/+$/, ''), BASE_URL, 'hub URL');
  const code = check(p.code, CODE, 'enrol code');
  const token = check(p.token, TOKEN, 'token');
  const setupArgs = einvoiceSetupArgs(p.platformUrl, { sqlPrompt: p.sqlPrompt }).join(' ');
  const replace = EINVOICE_GO_SERVICES.map((s) => `'${s}'`).join(',');
  const channel = p.canary ? ' -Channel canary' : '';
  return [
    '# Azolo E-Invoice: switch this PC from the MM-EInvoice agent to dl-agent',
    '$ErrorActionPreference = "Stop"',
    '[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072',
    `$env:DL_AGENT_EINVOICE_TOKEN = '${token}'`,
    `$env:DL_AGENT_SETUP_ARGS = '${setupArgs}'`,
    'try {',
    `    & ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('${hub}/install.ps1'))) -Code '${code}' -Setup einvoice-enroll -Replace ${replace}${channel}`,
    '} finally {',
    '    Remove-Item Env:\\DL_AGENT_EINVOICE_TOKEN -ErrorAction SilentlyContinue',
    '    Remove-Item Env:\\DL_AGENT_SETUP_ARGS -ErrorAction SilentlyContinue',
    '}',
    '',
  ].join('\r\n');
}

/**
 * The bench step (`POST /admin/bench`) that enrols the `einvoice` module against the prompted SQL Server.
 * The bench already moved the Go agent's folder to `MM-EInvoice.bench-old-<time>`; `einvoice-enroll`
 * reads the connection and collect window from there when the prompt gives none.
 */
export function einvoiceBenchStep(platformUrl: string, token: string): BenchStep {
  return {
    setup: 'einvoice-enroll',
    args: einvoiceSetupArgs(platformUrl),
    env: {
      DL_AGENT_EINVOICE_TOKEN: check(token, TOKEN, 'token'),
      MMEI_SQL_HOST: '{{SQL_SERVER}}',
      MMEI_SQL_USER: '{{SQL_USER}}',
      MMEI_SQL_PASSWORD: '{{SQL_PASSWORD}}',
      MMEI_SQL_AUTH: '{{SQL_AUTH}}',
    },
  };
}

/**
 * The module's hub commands (`POST /admin/commands`, `module: "einvoice"`), in the order the SR bench runs
 * them. None writes to SQL Server or the platform except `collect_now`, which is left out on purpose.
 */
export const EINVOICE_CHECK_COMMANDS = [
  { kind: 'einvoice.status', args: {} },
  { kind: 'einvoice.selftest', args: {} },
  { kind: 'einvoice.collect_dry_run', args: { sources: 'FO,POS', limit: 20 } },
  { kind: 'einvoice.writeback_probe', args: {} },
] as const;
