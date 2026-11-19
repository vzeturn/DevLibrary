import { describe, expect, it } from 'vitest';
import { benchStep } from '../src/domain/bench';
import { installScript, legacyScript } from '../src/install/script';
import {
  EINVOICE_CHECK_COMMANDS,
  EINVOICE_GO_SERVICES,
  einvoiceBenchStep,
  einvoicePilotScript,
  einvoiceSetupArgs,
} from '../src/install/einvoice';

const PILOT = {
  hub: 'https://agent.example.test/',
  code: 'ABCD-1234',
  platformUrl: 'https://einvoice.example.test',
  token: 'tok_SECRET-123',
};

describe('einvoice switch helpers', () => {
  it('passes --switch and never the token in the arguments', () => {
    expect(einvoiceSetupArgs('https://einvoice.example.test/')).toEqual(['--platform-url', 'https://einvoice.example.test', '--switch']);
    expect(einvoiceSetupArgs('https://e.test', { sqlPrompt: true })).toContain('--sql-prompt');
  });

  it('builds a PowerShell 2.0 pilot script that replaces the Go agent', () => {
    const s = einvoicePilotScript(PILOT);
    expect(s).toContain("$env:DL_AGENT_EINVOICE_TOKEN = 'tok_SECRET-123'");
    expect(s).toContain("$env:DL_AGENT_SETUP_ARGS = '--platform-url https://einvoice.example.test --switch'");
    expect(s).toContain("DownloadString('https://agent.example.test/install.ps1'))) -Code 'ABCD-1234' -Setup einvoice-enroll");
    expect(s).toContain(`-Replace ${EINVOICE_GO_SERVICES.map((p) => `'${p}'`).join(',')}`);
    expect(s).toContain('Remove-Item Env:\\DL_AGENT_EINVOICE_TOKEN');
    expect(s.split('DL_AGENT_SETUP_ARGS')[1]).not.toContain('tok_SECRET');
    expect(s).not.toMatch(/Invoke-RestMethod|\birm\b|\$\{/);
    expect(einvoicePilotScript({ ...PILOT, canary: true, sqlPrompt: true })).toMatch(/--sql-prompt'[\s\S]*-Channel canary/);
  });

  it.each([
    ['hub', { hub: "https://x.test'; Remove-Item C:\\" }],
    ['code', { code: "A'B" }],
    ['token', { token: 'a b' }],
    ['platform', { platformUrl: 'http://plain.test' }],
    ['platform', { platformUrl: 'https://x.test/$(calc)' }],
  ])('refuses an unsafe %s', (_what, over) => {
    expect(() => einvoicePilotScript({ ...PILOT, ...over })).toThrow(/einvoice: invalid/);
  });

  it('makes a bench step the bench schema accepts', () => {
    const step = einvoiceBenchStep('https://einvoice.example.test', 'tok-xyz');
    expect(benchStep.parse(step)).toEqual(step);
    expect(step.args).toContain('--switch');
    expect(step.args.join(' ')).not.toContain('tok-xyz');
    expect(step.env.MMEI_SQL_AUTH).toBe('{{SQL_AUTH}}');
  });

  it('has install.ps1 (thin and legacy) remove the token after the setup ran', () => {
    for (const text of [installScript('https://agent.example.test'), legacyScript('https://agent.example.test')]) {
      const list = text.match(/\$SetupSecrets = @\(([^)]*)\)/);
      expect(list?.[1]).toContain("'DL_AGENT_EINVOICE_TOKEN'");
      expect(list?.[1]).toContain("'DL_AGENT_PMS_CODE'");
      expect(text).toMatch(/foreach \(\$n in \(?(\$HandoffSecrets \+ )?\$SetupSecrets\)?\) \{ Remove-Item \('Env:' \+ \$n\)/);
    }
  });

  it('lists only read-only check commands', () => {
    const kinds = EINVOICE_CHECK_COMMANDS.map((c) => c.kind);
    expect(kinds).toEqual(['einvoice.status', 'einvoice.selftest', 'einvoice.collect_dry_run', 'einvoice.writeback_probe']);
    for (const k of kinds) expect(k).toMatch(/^[a-z][a-z0-9_.]{0,63}$/);
  });
});
