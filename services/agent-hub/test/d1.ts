// A D1 look-alike over node:sqlite, so repos run their real SQL against the real migrations.
import { readdirSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { DatabaseSync as Sqlite } from 'node:sqlite';

// Vite does not know the node:sqlite builtin, so load it through require.
const { DatabaseSync } = createRequire(import.meta.url)('node:sqlite') as { DatabaseSync: typeof Sqlite };

const MIGRATIONS = join(fileURLToPath(import.meta.url), '..', '..', 'migrations');

class Stmt {
  constructor(
    private db: Sqlite,
    private sql: string,
    private params: unknown[] = [],
  ) {}
  bind(...params: unknown[]): Stmt {
    return new Stmt(this.db, this.sql, params);
  }
  exec() {
    const stmt = this.db.prepare(this.sql);
    const reads = /^\s*(SELECT|WITH)/i.test(this.sql) || /\bRETURNING\b/i.test(this.sql);
    if (reads) {
      const results = stmt.all(...(this.params as never[]));
      return { success: true, results, meta: { changes: results.length, last_row_id: 0 } };
    }
    const info = stmt.run(...(this.params as never[]));
    return { success: true, results: [], meta: { changes: Number(info.changes), last_row_id: Number(info.lastInsertRowid) } };
  }
  async run() {
    return this.exec();
  }
  async all() {
    return this.exec();
  }
  async first(column?: string) {
    const row = (this.exec().results[0] ?? null) as Record<string, unknown> | null;
    return row && column ? (row[column] ?? null) : row;
  }
}

export function fakeD1() {
  const db = new DatabaseSync(':memory:');
  db.exec('PRAGMA foreign_keys = ON');
  for (const file of readdirSync(MIGRATIONS).filter((f) => f.endsWith('.sql')).sort()) {
    db.exec(readFileSync(join(MIGRATIONS, file), 'utf8'));
  }
  return {
    raw: db,
    prepare: (sql: string) => new Stmt(db, sql),
    async batch(stmts: Stmt[]) {
      db.exec('BEGIN');
      try {
        const out = stmts.map((s) => s.exec());
        db.exec('COMMIT');
        return out;
      } catch (error) {
        db.exec('ROLLBACK');
        throw error;
      }
    },
  };
}

export function fakeR2() {
  const store = new Map<string, Uint8Array>();
  return {
    store,
    async get(key: string) {
      const bytes = store.get(key);
      return bytes ? { body: new Response(bytes).body, size: bytes.byteLength } : null;
    },
    async put(key: string, value: Uint8Array | ArrayBuffer | string) {
      const bytes = typeof value === 'string' ? new TextEncoder().encode(value) : new Uint8Array(value);
      store.set(key, bytes);
      return { key, size: bytes.byteLength };
    },
    async delete(key: string) {
      store.delete(key);
    },
  };
}
