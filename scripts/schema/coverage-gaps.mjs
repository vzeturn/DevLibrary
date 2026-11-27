#!/usr/bin/env node
// Phase 3 of the SMILE schema coverage (RT-1216): turns the POST /admin/schema/coverage answer into a
// de-duplicated gap list, one entry per object, ranked by how many sites miss it. Structure names only.
//   node scripts/schema/coverage-gaps.mjs < coverage-answer.json
// Prints {"gaps": [...], "explained": [...]}. File each gap as an RT in smile-pms (pms:*) or pda-order
// (pda-sync:*) with the marker "[schema-gap:<object>]" in the description; skip entries whose marker
// already exists. "explained" lists objects a site lacks by design (cell status optional_absent, with the
// catalog's reason, ADR 0184): file nothing for them. Re-run when a site's schema_hash changes.
import { readFileSync } from 'node:fs';

const { sites } = JSON.parse(readFileSync(0, 'utf8'));
const gaps = new Map();
const explained = new Map();
for (const site of sites) {
  for (const c of site.cells) {
    if (c.status === 'present') continue;
    if (c.status === 'optional_absent') {
      const e = explained.get(c.object) ?? { object: c.object, status: c.status, reason: c.reason, features: new Set(), agents: new Set() };
      e.features.add(c.feature);
      e.agents.add(site.agent_id);
      explained.set(c.object, e);
      continue;
    }
    const key = `${c.object}|${c.status}|${c.missing_columns.join(',')}`;
    const g = gaps.get(key) ?? { marker: `[schema-gap:${c.object}]`, object: c.object, status: c.status, missing_columns: c.missing_columns, features: new Set(), agents: new Set() };
    g.features.add(c.feature);
    g.agents.add(site.agent_id);
    gaps.set(key, g);
  }
}
const bySites = (a, b) => b.sites - a.sites || a.object.localeCompare(b.object);
const out = {
  gaps: [...gaps.values()]
    .map((g) => ({ marker: g.marker, object: g.object, status: g.status, missing_columns: g.missing_columns, features: [...g.features], sites: g.agents.size, project: [...g.features].every((f) => f.startsWith('pda-sync')) ? 'pda-order' : 'smile-pms', priority: g.agents.size >= 3 ? 'high' : g.agents.size === 2 ? 'normal' : 'low' }))
    .sort(bySites),
  explained: [...explained.values()].map((e) => ({ object: e.object, status: e.status, reason: e.reason, features: [...e.features], sites: e.agents.size })).sort(bySites),
};
process.stdout.write(`${JSON.stringify(out, null, 1)}\n`);
