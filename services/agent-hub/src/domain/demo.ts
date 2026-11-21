import { z } from 'zod';

/**
 * Demo bench (RT-0261): `install.ps1 -Demo` installs dl-agent on canary for a demo tenant, then
 * enrols every product module with that product's own demo tenant and runs the self test.
 *
 * The hub mints only the dl-agent enrolment code. Every module code comes from another cloud
 * (E-Invoice, SMILE PMS, Check-In, Membership, Hotel Link, SRBackup, PDA), so the operator
 * supplies it on the machine through the variables below (env, or `-DemoEnv @{…}`); the script
 * hands it to the module's enrol tool through the variable the tool already reads, never argv,
 * and removes it afterwards. This table is the single source: the hub answers it from
 * `POST /admin/demo-bench` and renders it into `install.ps1` (`demoModulesPs`).
 */

/** A secret the operator supplies (`from`), handed to the tool as the variable it reads (`to`). */
export interface DemoSecret {
  from: string;
  to: string;
  required: boolean;
  what: string;
}

/** A plain option (URL, id) the operator supplies (`from`), passed as `flag <value>`. */
export interface DemoOption {
  from: string;
  flag: string;
  required: boolean;
  what: string;
}

/** A flag without a value the tool takes (remote attach `flags`). */
export interface DemoFlag {
  flag: string;
  what: string;
}

export interface DemoModule {
  /** The module name in the agent config (`modules.<module>.enabled`). */
  module: string;
  product: string;
  /** The dl-agent enrol sub-command; null for a module with nothing to enrol. */
  setup: string | null;
  secrets: DemoSecret[];
  options: DemoOption[];
  /** Value-less flags remote attach may pass (`--force`, `--sql-admin-saved`). */
  flags: DemoFlag[];
  /** The flag that enrols again over an existing enrolment (another tenant), null when none. */
  reenroll: string | null;
  /**
   * Remote attach only: the module's config keys naming the files or directories the module reads its
   * enrolment from, each with the tool flag that makes the tool write there (else it writes to its own
   * default). Empty for a module that enrols into the agent data dir.
   */
  dirs: DemoDir[];
  note: string;
}

export interface DemoDir {
  key: string;
  flag: string;
}

const s = (from: string, to: string, what: string, required = true): DemoSecret => ({ from, to, required, what });
const o = (from: string, flag: string, what: string, required = true): DemoOption => ({ from, flag, required, what });
const FORCE: DemoFlag = { flag: '--force', what: 'enrol again although the module is enrolled (moves it to the new tenant)' };

/** In the order the script enrols them (`speaker` needs `pda-sync` first). */
export const DEMO_MODULES: readonly DemoModule[] = [
  {
    module: 'einvoice',
    product: 'Azolo E-Invoice',
    setup: 'einvoice-enroll',
    secrets: [s('DL_AGENT_DEMO_EINVOICE_TOKEN', 'DL_AGENT_EINVOICE_TOKEN', 'one-time agent token (platform console, demo tenant, Agent tab)')],
    options: [o('DL_AGENT_DEMO_EINVOICE_URL', '--platform-url', 'E-Invoice platform origin')],
    flags: [FORCE],
    reenroll: '--force',
    dirs: [{ key: 'config_dir', flag: '--config-dir' }],
    note: 'SQL login from MMEI_SQL_HOST/USER/PASSWORD/AUTH when set, else the tool defaults',
  },
  {
    module: 'pms-site',
    product: 'SMILE PMS',
    setup: 'pms-enroll',
    secrets: [s('DL_AGENT_DEMO_PMS_CODE', 'DL_AGENT_PMS_CODE', 'one-time site enrolment code (PMS cloud, demo hotel)')],
    options: [o('DL_AGENT_DEMO_PMS_URL', '--cloud-url', 'PMS cloud origin')],
    flags: [FORCE],
    reenroll: '--force',
    dirs: [
      { key: 'config_path', flag: '--config' },
      { key: 'state_dir', flag: '--state-dir' },
    ],
    note: 'the module redeems the code within 30 s',
  },
  {
    module: 'arrival',
    product: 'Azolo Check-In',
    setup: 'arrival-enroll',
    secrets: [
      s('DL_AGENT_DEMO_ARRIVAL_TOKEN', 'DL_AGENT_ARRIVAL_TOKEN', 'enrolment token (platform console, demo hotel)'),
      s('DL_AGENT_DEMO_ARRIVAL_PMS', 'DL_AGENT_ARRIVAL_PMS', 'PMS connection string (else the saved one, the old agent one, or the core sql.json SMILE_FO)', false),
    ],
    options: [o('DL_AGENT_DEMO_ARRIVAL_URL', '--cloud-url', 'Check-In cloud origin')],
    flags: [FORCE],
    reenroll: '--force',
    dirs: [{ key: 'install_dir', flag: '--install-dir' }],
    note: '',
  },
  {
    module: 'membership',
    product: 'Membership',
    setup: 'membership-enroll',
    secrets: [
      s('DL_AGENT_DEMO_MEMBERSHIP_TOKEN', 'DL_AGENT_MEMBERSHIP_TOKEN', 'one-time agent code (Membership admin, demo hotel)'),
      s('DL_AGENT_DEMO_MEMBERSHIP_SMILE', 'DL_AGENT_MEMBERSHIP_SMILE', 'SMILE connection string (else the saved one)', false),
    ],
    options: [o('DL_AGENT_DEMO_MEMBERSHIP_URL', '--hub-url', 'Membership hub origin')],
    flags: [FORCE],
    reenroll: '--force',
    dirs: [{ key: 'program_data_dir', flag: '--config-dir' }],
    note: '',
  },
  {
    module: 'hotellink',
    product: 'SMILE Hotel Link',
    setup: 'hl-enroll',
    secrets: [
      s('DL_AGENT_DEMO_HL_TOKEN', 'DL_AGENT_HL_TOKEN', 'hotel token (HL hub dashboard, demo hotel)'),
      s('DL_AGENT_DEMO_HL_SQL_PASSWORD', 'DL_AGENT_HL_SQL_PASSWORD', 'SQL password, with DL_AGENT_DEMO_HL_SQL_USER', false),
    ],
    options: [
      o('DL_AGENT_DEMO_HL_URL', '--hub-url', 'HL hub origin'),
      o('DL_AGENT_DEMO_HL_HOTEL_ID', '--hotel-id', 'hotel id on the HL hub'),
      o('DL_AGENT_DEMO_HL_INSTANCE_ID', '--instance-id', 'instance id of this install'),
      o('DL_AGENT_DEMO_HL_SQL_USER', '--sql-user', 'SQL login (default: Windows authentication)', false),
    ],
    flags: [FORCE],
    reenroll: '--force',
    dirs: [],
    note: '',
  },
  {
    module: 'backup',
    product: 'SRBackup',
    setup: 'backup-enroll',
    secrets: [s('DL_AGENT_DEMO_BACKUP_KEY', 'DL_AGENT_BACKUP_KEY', 'enroll key (SRBackup hub, demo tenant)')],
    options: [o('DL_AGENT_DEMO_BACKUP_URL', '--hub-url', 'SRBackup hub origin')],
    flags: [FORCE],
    reenroll: '--force',
    dirs: [],
    note: '',
  },
  {
    module: 'pda-sync',
    product: 'PDAOrder',
    setup: 'pda-enroll',
    secrets: [
      s('DL_AGENT_DEMO_PDA_CODE', 'DL_AGENT_PDA_CODE', 'one-time enrol code (PDA admin, demo tenant)'),
      s('DL_AGENT_DEMO_PDA_SQL_ADMIN_PASSWORD', 'DL_AGENT_PDA_SQL_ADMIN_PASSWORD', 'SQL administrator password, with DL_AGENT_DEMO_PDA_SQL_ADMIN_USER', false),
      s('DL_AGENT_DEMO_PDA_SQL_DEFAULT_PASSWORD', 'DL_AGENT_PDA_SQL_DEFAULT_PASSWORD', 'SMILE default SQL login password (default: the one built into dl-agent)', false),
    ],
    options: [
      o('DL_AGENT_DEMO_PDA_URL', '--url', 'the demo tenant PDA cloud address'),
      o('DL_AGENT_DEMO_PDA_SQL_ADMIN_USER', '--sql-admin-user', 'SQL administrator login (default: the Windows account)', false),
    ],
    flags: [{ flag: '--sql-admin-saved', what: 'use the SQL login dl-agent saved in sql.json as the SQL administrator' }],
    reenroll: null,
    dirs: [],
    note: 'needs a dl-agent build that reads DL_AGENT_PDA_CODE (RT-0261)',
  },
  {
    module: 'pms-device',
    product: 'SMILE PMS devices',
    setup: 'pms-device-enroll',
    secrets: [s('DL_AGENT_DEMO_PMS_DEVICE_TOKEN', 'DL_AGENT_PMS_DEVICE_TOKEN', 'device agent token (else generated; register its sha256 in the PMS)', false)],
    options: [
      o('DL_AGENT_DEMO_PMS_DEVICE_URL', '--api-url', 'PMS API origin'),
      o('DL_AGENT_DEMO_PMS_DEVICE_AGENT_ID', '--agent-id', 'device agent id (default: computer name)', false),
    ],
    flags: [FORCE],
    reenroll: '--force',
    dirs: [{ key: 'appsettings_dir', flag: '--appsettings-dir' }],
    note: 'writes AgentId, TokenFile and ApiBaseUrl; prints the token sha256 to register in the PMS',
  },
  {
    module: 'speaker',
    product: 'PDAOrder speaker',
    setup: null,
    secrets: [],
    options: [],
    flags: [],
    reenroll: null,
    dirs: [],
    note: 'no enrol step: uses the pda-sync identity',
  },
];

export const DEMO_MODULE_NAMES = DEMO_MODULES.map((m) => m.module);

/** Every operator variable, secret or not (the script clears them all when it ends). */
export function demoVariables(mods: readonly DemoModule[] = DEMO_MODULES): string[] {
  return mods.flatMap((m) => [...m.secrets.map((x) => x.from), ...m.options.map((x) => x.from)]);
}

const SECRET_VARS = new Set(DEMO_MODULES.flatMap((m) => m.secrets.map((x) => x.from)));
const OPTION_VARS = new Set(DEMO_MODULES.flatMap((m) => m.options.map((x) => x.from)));
const ID = /^[A-Za-z0-9._-]{1,64}$/;
/** Plain option values the hub may prefill: an https origin for `*_URL`, an id otherwise. */
const optionValue = z.string().max(500).regex(/^(https:\/\/[A-Za-z0-9.-]+(:\d{1,5})?(\/[A-Za-z0-9._~\/-]*)?|[A-Za-z0-9._@\\-]{1,128})$/, 'an https URL or an id');

const id = z.string().regex(ID, 'letters, digits, . _ - only');

export const demoBenchBody = z
  .object({
    tenant_id: id,
    site_id: id,
    /** Default: every module. */
    modules: z
      .array(z.enum(DEMO_MODULE_NAMES as [string, ...string[]]))
      .min(1)
      .max(DEMO_MODULES.length)
      .optional(),
    expires_hours: z.number().int().min(1).max(72).default(24),
    /**
     * Non-secret option values to prefill in the answer's `operator_env` (URLs, hotel id …).
     * A secret variable here is refused: module codes never pass through the hub.
     */
    settings: z.record(z.string(), optionValue).default({}),
  })
  .superRefine((b, ctx) => {
    for (const name of Object.keys(b.settings)) {
      if (SECRET_VARS.has(name)) {
        ctx.addIssue({ code: 'custom', path: ['settings', name], message: 'a secret: set it on the machine, never through the hub' });
      } else if (!OPTION_VARS.has(name)) {
        ctx.addIssue({ code: 'custom', path: ['settings', name], message: 'not a demo option variable' });
      } else if (name.endsWith('_URL') && !b.settings[name]?.startsWith('https://')) {
        ctx.addIssue({ code: 'custom', path: ['settings', name], message: 'an https URL' });
      }
    }
    if (b.modules && new Set(b.modules).size !== b.modules.length) {
      ctx.addIssue({ code: 'custom', path: ['modules'], message: 'listed twice' });
    }
  });
export type DemoBenchBody = z.infer<typeof demoBenchBody>;

/** The selected modules in catalogue order. */
export function selectDemoModules(names: readonly string[] | undefined): DemoModule[] {
  return names ? DEMO_MODULES.filter((m) => names.includes(m.module)) : [...DEMO_MODULES];
}

/** A PowerShell single-quoted literal (typographic quotes count as quotes in PowerShell). */
export function psLiteral(v: string): string {
  return `'${v.replace(/['‘’‚‛]/g, (q) => q + q)}'`;
}

const psList = (xs: readonly string[]): string => `@(${xs.map(psLiteral).join(', ')})`;

/** The `$DemoTable` block of `install.ps1`: one hashtable per module, PowerShell 2.0 syntax. */
export function demoModulesPs(mods: readonly DemoModule[] = DEMO_MODULES): string {
  const rows = mods.map((m) =>
    [
      `  @{ Module = ${psLiteral(m.module)}; Setup = ${psLiteral(m.setup ?? '')}`,
      `SecretFrom = ${psList(m.secrets.map((x) => x.from))}; SecretTo = ${psList(m.secrets.map((x) => x.to))}`,
      `SecretNeed = ${psList(m.secrets.filter((x) => x.required).map((x) => x.from))}`,
      `OptionFrom = ${psList(m.options.map((x) => x.from))}; OptionFlag = ${psList(m.options.map((x) => x.flag))}`,
      `OptionNeed = ${psList(m.options.filter((x) => x.required).map((x) => x.from))}; Note = ${psLiteral(m.note)} }`,
    ].join('; '),
  );
  return `$DemoTable = @(\n${rows.join(',\n')}\n)`;
}
