#!/usr/bin/env node
// Usage: node scripts/latency-probe/probe.mjs [--samples 200] [--gap 150] [--only name,name]
// Times each route below (5 warm-ups dropped, N sequential samples, one 10-parallel burst) and prints
// one JSON report. GET routes only: never add a write here (pos.check.pay has no dry-run, RT-1161).
import { foLogin, posLogin, timed, summarize } from './lib.mjs';

// route -> the dl-agent ops it fans out to on an agent tenant (azolo-pms apps/api).
export const ROUTES = [
  { name: 'health', auth: null, path: '/../../health', ops: [] },
  { name: 'fo.hotelDate.get', auth: 'pos', path: '/pos/hotel-date', ops: ['fo.hotelDate.get'] },
  { name: 'pos.menu.items', auth: 'pos', path: '/pos/menu/items', ops: ['pos.menu.items'] },
  { name: 'pos.check.list', auth: 'pos', path: '/pos/terminal/checks', ops: ['pos.check.list'] },
  { name: 'fo.inhouse.list', auth: 'fo', path: '/workspace/frontdesk/today?blocks=inHouse', ops: ['fo.hotelDate.get', 'fo.inhouse.list'] },
  { name: 'fo.reservation.list', auth: 'fo', path: '/reservations', ops: ['fo.hotelDate.get', 'fo.reservation.list', 'fo.inhouse.list'] },
  { name: 'frontdesk.today', auth: 'fo', path: '/workspace/frontdesk/today', ops: ['fo.hotelDate.get', 'fo.arrivals.expected', 'fo.departures.expected', 'fo.inhouse.list'] },
  { name: 'dashboard.overview', auth: 'fo', path: '/dashboard/overview', ops: ['fo.dashboard.today', 'fo.hotelDate.get', 'fo.arrivals.expected', 'fo.departures.expected', 'fo.inhouse.list', 'fo.dashboard.houseMix', 'fo.dashboard.rooms'] },
];

const args = process.argv.slice(2);
const opt = (k, d) => { const i = args.indexOf('--' + k); return i >= 0 ? args[i + 1] : d; };
const N = Number(opt('samples', '200'));
const GAP = Number(opt('gap', '150'));
const only = opt('only', '')?.split(',').filter(Boolean);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// A gap past the API's 5 s read cache also outlives undici's 4 s keep-alive, so every sample would pay a
// fresh TLS handshake. Long gaps keep the connection warm with untimed /health calls.
async function pause(ms) {
  for (let left = ms; left > 0; left -= 2000) {
    await sleep(Math.min(2000, left));
    if (ms > 3000 && left > 2000) await timed('/../../health');
  }
}

const tokens = {};
async function token(auth, force = false) {
  if (!auth) return undefined;
  if (force || !tokens[auth]) tokens[auth] = auth === 'pos' ? await posLogin() : await foLogin();
  return tokens[auth];
}
async function one(route) {
  let r = await timed(route.path, await token(route.auth));
  if (r.status === 401 && route.auth) r = await timed(route.path, await token(route.auth, true));
  return r;
}

const report = { api: process.env.LATENCY_PROBE_API ?? 'azolo-demo-pms', start: new Date().toISOString(), samples: N, gapMs: GAP, routes: [] };
for (const route of ROUTES.filter((r) => !only?.length || only.includes(r.name))) {
  const out = { name: route.name, path: route.path, ops: route.ops, errors: {} };
  const note = (r) => { const k = r.status + (r.code ? ' ' + r.code : ''); out.errors[k] = (out.errors[k] ?? 0) + 1; };
  for (let i = 0; i < 5; i++) await one(route);
  const seq = [];
  let streak = 0;
  for (let i = 0; i < N; i++) {
    const r = await one(route);
    if (r.status < 400) { seq.push(r.ms); streak = 0; } else { note(r); if (++streak >= 5) { out.stopped = 'five errors in a row'; break; } }
    if (r.ms > 5000) { out.stopped = 'sample over 5 s'; break; }
    await pause(GAP);
  }
  out.seq = summarize(seq);
  const burst = await Promise.all(Array.from({ length: 10 }, () => one(route)));
  burst.filter((r) => r.status >= 400).forEach(note);
  out.par10 = summarize(burst.filter((r) => r.status < 400).map((r) => r.ms));
  report.routes.push(out);
  console.error(`${route.name}: n=${out.seq.n} p50=${out.seq.p50} p95=${out.seq.p95}`);
}
report.end = new Date().toISOString();
console.log(JSON.stringify(report, null, 1));
