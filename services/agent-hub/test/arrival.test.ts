import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { ARRIVAL_LEGACY_SQL_BODY, ARRIVAL_OLD_DIR, ARRIVAL_SQL_SOURCE } from '../src/install/arrival';
import { benchScript } from '../src/install/benchScript';

const script = (): string =>
  benchScript({
    hub: 'https://hub.test',
    label: 'x',
    tenant_id: 'aurelia-demo',
    site_id: 'sr',
    expires_at: 1_800_000_000,
    code: 'ABCD-EFGH-JKLM-NPQR',
    steps: [],
    sql_databases: ['SMILE_FO', 'SMILE_PICTURE'],
    sql_fixups: [],
    sql_names: ['SMILE_FO', 'SMILE_PICTURE'],
    sql_parts: false,
    remove_legacy: true,
    sql_grant_service: false,
  });

const pwsh = ((): boolean => {
  try {
    execFileSync('pwsh', ['-NoProfile', '-Command', '1'], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
})();

describe('bench script: the old Check-In agent SQL connection (arrival.ts)', () => {
  it('is spliced in once, free of backticks and dollar-brace', () => {
    expect(ARRIVAL_LEGACY_SQL_BODY).not.toMatch(/`|\$\{/);
    const s = script();
    expect(s).not.toContain('__ARRIVAL__');
    expect(s.split('function Get-ArrivalLegacySql').length).toBe(2);
    expect(s).toContain(`'${ARRIVAL_OLD_DIR}.bench-old-*'`);
  });

  it.skipIf(!pwsh)('prefills server and authentication from the old or renamed folder; sql.json wins', () => {
    const lifted = /^# The server part of the connection[\s\S]*?(?=^# Asks for SQL Server)/m.exec(script())?.[0] ?? '';
    expect(lifted).toContain('function Get-ArrivalLegacySql');
    const dir = mkdtempSync(join(tmpdir(), 'bench-arrival-'));
    try {
      const probe = join(dir, 'probe.ps1');
      writeFileSync(
        probe,
        [
          '$St = @{ Secrets = (New-Object Collections.ArrayList); SqlFrom = $null }',
          'function Add-Secret([string]$s) { if ($s) { [void]$St.Secrets.Add($s) } }',
          'function Warn([string]$m) { [void]$St.Secrets.Add("warn") }',
          '$Data = $args[0]',
          '$env:ProgramW6432 = $args[1]',
          lifted,
          '$s = Get-SavedSql',
          "@{ saved = $s; show = $(if ($s) { Show-Sql $s } else { '' }); secrets = $St.Secrets.Count; from = [string]$St.SqlFrom } | ConvertTo-Json -Compress",
        ].join('\n'),
      );
      const settings = (conn: string): string => JSON.stringify({ Agent: { PmsConnectionString: conn, CloudBaseUrl: 'https://x' } }, null, 2);
      const run = (folders: Record<string, string>, sqlJson?: string): Record<string, string | number> => {
        const root = mkdtempSync(join(dir, 'pf-'));
        const data = mkdtempSync(join(dir, 'data-'));
        for (const [name, body] of Object.entries(folders)) {
          mkdirSync(join(root, name));
          writeFileSync(join(root, name, 'appsettings.Production.json'), body);
        }
        if (sqlJson !== undefined) writeFileSync(join(data, 'sql.json'), sqlJson);
        return JSON.parse(execFileSync('pwsh', ['-NoProfile', '-File', probe, data, root]).toString()) as Record<string, string | number>;
      };

      const login = run({ [ARRIVAL_OLD_DIR]: settings('Server=HOTEL\\SQL;Database=SMILE_FO;User Id=ci;Password="p;w""x";TrustServerCertificate=True') });
      expect(login.show).toBe('HOTEL\\SQL, SQL login ci');
      expect(login.saved).not.toMatch(/SMILE_FO|Catalog|Database/i);
      expect(login.saved).toContain('p;w"x');
      expect(login.secrets).toBe(1);
      expect(login.from).toContain(ARRIVAL_SQL_SOURCE);

      // remove_legacy ran first: the newest renamed folder is the one read.
      const renamed = run({
        [`${ARRIVAL_OLD_DIR}.bench-old-20261001080000`]: settings('Server=OLDHOST;Database=SMILE_FO;Integrated Security=true'),
        [`${ARRIVAL_OLD_DIR}.bench-old-20261119080000`]: settings('Server=.;Database=SMILE_FO;Integrated Security=true'),
      });
      expect(renamed.show).toBe('., Windows authentication');
      expect(renamed.from).toContain('bench-old-20261119080000');
      expect(renamed.secrets).toBe(0);

      const kept = run(
        { [ARRIVAL_OLD_DIR]: settings('Server=HOTEL;Database=SMILE_FO;Integrated Security=true') },
        JSON.stringify({ databases: { SMILE_FO: { connection: 'Server=NEW;User Id=sa;Password=q;Database=SMILE_FO;' } } }),
      );
      expect(kept.show).toBe('NEW, SQL login sa');
      expect(kept.from).toBe('');

      expect(run({}).saved).toBe('');
      expect(run({ [ARRIVAL_OLD_DIR]: '{"Agent":{}}' }).saved).toBe('');
      const broken = run({ [ARRIVAL_OLD_DIR]: '{"Agent":{"PmsConnectionString":"Server=a;Bogus Key=1"}}' });
      expect(broken.saved).toBe('');
      expect(broken.secrets).toBe(1); // the Warn, not a secret
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
