export interface ProductKeyRow {
  id: string;
  product: string;
  key_hash: string;
  label: string;
  created_at: number;
  last_used: number | null;
  revoked_at: number | null;
}

export type ProductKeyView = Omit<ProductKeyRow, 'key_hash'>;

export async function insertProductKey(db: D1Database, row: Omit<ProductKeyRow, 'last_used' | 'revoked_at'>): Promise<void> {
  await db
    .prepare('INSERT INTO product_keys (id, product, key_hash, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5)')
    .bind(row.id, row.product, row.key_hash, row.label, row.created_at)
    .run();
}

/** The live key with this hash, or null when unknown or revoked. */
export async function findLiveKey(db: D1Database, hash: string): Promise<ProductKeyRow | null> {
  return db.prepare('SELECT * FROM product_keys WHERE key_hash = ?1 AND revoked_at IS NULL').bind(hash).first<ProductKeyRow>();
}

export async function touchProductKey(db: D1Database, id: string, now: number): Promise<void> {
  await db.prepare('UPDATE product_keys SET last_used = ?2 WHERE id = ?1').bind(id, now).run();
}

export async function listProductKeys(db: D1Database): Promise<ProductKeyView[]> {
  const sql = 'SELECT id, product, label, created_at, last_used, revoked_at FROM product_keys ORDER BY created_at DESC';
  return (await db.prepare(sql).all<ProductKeyView>()).results;
}

export async function revokeProductKey(db: D1Database, id: string, now: number): Promise<boolean> {
  const res = await db.prepare('UPDATE product_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL').bind(id, now).run();
  return res.meta.changes === 1;
}
