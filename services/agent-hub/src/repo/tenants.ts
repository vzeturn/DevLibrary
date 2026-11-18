export interface TenantRow {
  id: string;
  name: string;
  demo: number;
  created_at: number;
  updated_at: number;
}

export async function upsertTenant(db: D1Database, t: { id: string; name: string; demo: boolean }, now: number): Promise<void> {
  await db
    .prepare(
      `INSERT INTO tenants (id, name, demo, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)
       ON CONFLICT (id) DO UPDATE SET name = excluded.name, demo = excluded.demo, updated_at = excluded.updated_at`,
    )
    .bind(t.id, t.name, t.demo ? 1 : 0, now)
    .run();
}

export async function getTenant(db: D1Database, id: string): Promise<TenantRow | null> {
  return db.prepare('SELECT * FROM tenants WHERE id = ?1').bind(id).first<TenantRow>();
}

export async function listTenants(db: D1Database): Promise<TenantRow[]> {
  return (await db.prepare('SELECT * FROM tenants ORDER BY id').all<TenantRow>()).results;
}

export async function ensureSite(db: D1Database, tenantId: string, siteId: string, now: number): Promise<void> {
  await db
    .prepare('INSERT INTO sites (tenant_id, id, created_at) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING')
    .bind(tenantId, siteId, now)
    .run();
}
