import type { Env } from '../env';
import { listReleases } from '../repo/releases';
import { agentsView } from '../services/admin';
import { gateStatus } from '../services/releases';

const ESC: Record<string, string> = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
export function esc(value: unknown): string {
  return String(value ?? '').replace(/[&<>"']/g, (ch) => ESC[ch] ?? ch);
}

function when(ts: number | null, now: number): string {
  if (ts === null) return 'chưa có';
  const ago = now - ts;
  if (ago < 60) return `${ago} giây trước`;
  if (ago < 3600) return `${Math.floor(ago / 60)} phút trước`;
  if (ago < 86400) return `${Math.floor(ago / 3600)} giờ trước`;
  return new Date(ts * 1000).toISOString().slice(0, 16).replace('T', ' ') + ' UTC';
}

const STYLE = `
  :root{color-scheme:light dark;--fg:#1d2329;--muted:#66707a;--line:#e3e6ea;--ok:#1a7f37;--bad:#c62828;--warn:#9a6700;--bg:#fff}
  @media (prefers-color-scheme:dark){:root{--fg:#e6e8eb;--muted:#9aa4ae;--line:#30363d;--bg:#0f1317;--ok:#3fb950;--bad:#f85149;--warn:#d29922}}
  body{margin:0;padding:24px 16px;font:14px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif;color:var(--fg);background:var(--bg)}
  main{max-width:1100px;margin:0 auto} h1{font-size:20px;margin:0 0 4px} h2{font-size:16px;margin:28px 0 8px}
  p.sub{color:var(--muted);margin:0 0 8px} .wrap{overflow-x:auto}
  table{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums}
  th,td{text-align:left;padding:6px 10px;border-bottom:1px solid var(--line);vertical-align:top;white-space:nowrap}
  th{color:var(--muted);font-weight:600;font-size:12px;text-transform:uppercase;letter-spacing:.03em}
  .ok{color:var(--ok)} .bad{color:var(--bad)} .warn{color:var(--warn)} .muted{color:var(--muted)}
  td.wrapcell{white-space:normal;min-width:240px}`;

export async function renderStatusPage(env: Env, now: number): Promise<string> {
  const agents = await agentsView(env, undefined, now);
  const releases = await listReleases(env.DB);
  const stable = new Set(releases.filter((r) => r.channel === 'stable').map((r) => `${r.product}@${r.version}`));

  const agentRows = agents
    .map((a) => {
      const state = a.revoked
        ? '<span class="muted">Đã thu hồi</span>'
        : a.online
          ? '<span class="ok">Trực tuyến</span>'
          : '<span class="bad">Ngoại tuyến</span>';
      const degraded = a.degraded_modules.length
        ? `<span class="warn">${esc(a.degraded_modules.join(', '))}</span>`
        : '<span class="muted">—</span>';
      return `<tr><td>${esc(a.tenant_id)}</td><td>${esc(a.site_id)}</td><td>${esc(a.hostname)}</td>
        <td>${esc(a.version)}</td><td>${esc(a.channel)}</td><td>${state}</td>
        <td>${esc(when(a.last_seen_at, now))}</td><td class="wrapcell">${degraded}</td></tr>`;
    })
    .join('');

  const releaseRows: string[] = [];
  for (const r of releases.filter((x) => x.channel === 'canary')) {
    let gate: string;
    if (stable.has(`${r.product}@${r.version}`)) {
      gate = '<span class="ok">Đã lên stable</span>';
    } else {
      const g = await gateStatus(env, r.product, r.version);
      const failing = g.checks.filter((c) => !c.ok).map((c) => esc(c.detail));
      gate = g.pass ? '<span class="ok">Đạt — có thể lên stable</span>' : `<span class="bad">Chưa đạt:</span> ${failing.join('; ')}`;
    }
    releaseRows.push(`<tr><td>${esc(r.product)}</td><td>${esc(r.version)}</td>
      <td>${esc(when(r.published_at, now))}</td><td class="wrapcell">${gate}</td></tr>`);
  }

  const empty = (cols: number, text: string): string => `<tr><td colspan="${cols}" class="muted">${text}</td></tr>`;
  return `<!doctype html><html lang="vi"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>Agent hub</title><style>${STYLE}</style></head>
<body><main>
<h1>Agent hub</h1><p class="sub">Trạng thái lúc ${esc(new Date(now * 1000).toISOString().slice(0, 19).replace('T', ' '))} UTC · ${agents.length} agent</p>
<h2>Agent</h2><div class="wrap"><table><thead><tr><th>Khách hàng</th><th>Chi nhánh</th><th>Máy</th><th>Phiên bản</th>
<th>Kênh</th><th>Trạng thái</th><th>Nhịp gần nhất</th><th>Module lỗi</th></tr></thead>
<tbody>${agentRows || empty(8, 'Chưa có agent nào.')}</tbody></table></div>
<h2>Bản phát hành (canary)</h2><div class="wrap"><table><thead><tr><th>Sản phẩm</th><th>Phiên bản</th><th>Phát hành</th>
<th>Cổng canary</th></tr></thead><tbody>${releaseRows.join('') || empty(4, 'Chưa có bản phát hành nào.')}</tbody></table></div>
</main></body></html>`;
}
