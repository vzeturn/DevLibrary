#!/usr/bin/env node
// Phase 3 of the SMILE schema coverage (RT-1216): turns the POST /admin/schema/coverage answer into a
// de-duplicated gap list, one entry per object, ranked by how many sites miss it. Structure names only.
//   node scripts/schema/coverage-gaps.mjs < coverage-answer.json
// File each entry as an RT in smile-pms (pms:*) or pda-order (pda-sync:*) with the marker
// "[schema-gap:<object>]" in the description; skip entries whose marker already exists. Re-run when a
// site's schema_hash changes.
import { readFileSync } from 'node:fs';

const { sites } = JSON.parse(readFileSync(0, 'utf8'));
const gaps = new Map();
for (const site of sites) {
  for (const c of site.cells) {
    if (c.status === 'present') continue;
    const key = `${c.object}|${c.status}|${c.missing_columns.join(',')}`;
    const g = gaps.get(key) ?? { marker: `[schema-gap:${c.object}]`, object: c.object, status: c.status, missing_columns: c.missing_columns, features: new Set(), agents: new Set() };
    g.features.add(c.feature);
    g.agents.add(site.agent_id);
    gaps.set(key, g);
  }
}
const out = [...gaps.values()]
  .map((g) => ({ marker: g.marker, object: g.object, status: g.status, missing_columns: g.missing_columns, features: [...g.features], sites: g.agents.size, project: [...g.features].every((f) => f.startsWith('pda-sync')) ? 'pda-order' : 'smile-pms', priority: g.agents.size >= 3 ? 'high' : g.agents.size === 2 ? 'normal' : 'low' }))
  .sort((a, b) => b.sites - a.sites || a.object.localeCompare(b.object));
process.stdout.write(`${JSON.stringify(out, null, 1)}\n`);
