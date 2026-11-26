#!/usr/bin/env node
// Builds the expected-objects list of the schema coverage matrix (RT-1215) from the manifests
// the agents already ship, so it never drifts from what the features really touch:
//   - dl-agent-pms ops manifest: every procedure (routine) and SELECT (table + columns) per op
//   - dl-agent-pda-sync catalog: every SMILE_POS table PDAOrder syncs (used_by_pda), per class
// Prints the body of POST /v1/admin/schema/coverage: {"expected": [...]}.
//   node scripts/schema/expected-objects.mjs [agent_id ...] > coverage.json
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const read = (p) => JSON.parse(readFileSync(join(root, p), 'utf8'));
const manifest = read('crates/dl-agent-pms/assets/ops-manifest.json');
const catalog = read('crates/dl-agent-pda-sync/assets/catalog/sync-catalog.json');

const expected = [];
const seen = new Set();
const add = (item) => {
  const key = JSON.stringify(item);
  if (!seen.has(key)) {
    seen.add(key);
    expected.push(item);
  }
};

for (const op of manifest.ops) {
  if (!op.target_db) continue;
  for (const call of op.calls ?? []) {
    if (call.proc) add({ feature: `pms:${op.name}`, database: op.target_db, routine: call.proc });
    if (call.select) {
      const columns = [...new Set([...(call.select.columns ?? []), ...(call.select.where ?? []).map((w) => w.column)])];
      add({ feature: `pms:${op.name}`, database: op.target_db, table: call.select.table, columns });
    }
  }
}
for (const t of catalog.tables) {
  if (t.used_by_pda) add({ feature: `pda-sync:class-${t.class}`, database: 'SMILE_POS', table: t.table });
}

const agentIds = process.argv.slice(2);
process.stdout.write(`${JSON.stringify(agentIds.length > 0 ? { expected, agent_ids: agentIds } : { expected }, null, 1)}\n`);
