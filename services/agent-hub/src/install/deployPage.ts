import { installCommand } from './script';

/** Placeholder the page swaps for the code it reads from the link's fragment. */
export const CODE_SLOT = '__CODE__';
/** Placeholder for a product's install script URL in the page's boot line. */
const BOOT_SLOT = '__BOOT__';

/** Display names of the products that mint codes (the module name is the product key). */
export const PRODUCT_LABELS: Record<string, string> = {
  einvoice: 'Azolo E-Invoice',
  arrival: 'Azolo Check-In',
  membership: 'Azolo Membership',
  'pms-site': 'SMILE PMS',
  'pms-device': 'SMILE PMS (thiết bị)',
  'pda-sync': 'PDAOrder',
  hotellink: 'Hotel Link',
  backup: 'SRBackup',
};

/**
 * Host suffixes a product console's own install script may come from (`#boot=` / `#alt=`): the Azolo
 * product domains. Anything else is refused so a crafted link cannot show a foreign script on the hub.
 */
export const BOOT_HOST_SUFFIXES = ['pms.id.vn', 'mmsolutions.vn'] as const;

/**
 * The product's own install script URL when it is safe to quote into PowerShell, else ''. A plain
 * query (letters, digits, `=&._-`, e.g. `?channel=canary`) is allowed; quotes, `%`, `$` and ports are not. Kept free
 * of closures and TypeScript-only syntax at runtime: the page embeds it verbatim (`toString`).
 */
export function safeBootUrl(raw: unknown, suffixes: readonly string[]): string {
  if (typeof raw !== 'string' || raw.length > 300) return '';
  var m = /^https:\/\/([a-z0-9.-]+)(\/[A-Za-z0-9._~\/-]*)(\?[A-Za-z0-9._=&-]*)?$/.exec(raw);
  var host = m ? m[1] || '' : '';
  if (!host || host.indexOf('..') >= 0) return '';
  for (var i = 0; i < suffixes.length; i++) {
    var s = suffixes[i] || '';
    if (!s) continue;
    if (host === s || host.slice(-(s.length + 1)) === '.' + s) return raw;
  }
  return '';
}

/** The line that runs a product's own install script (PowerShell 2.0 safe, TLS 1.2 on). */
export function bootCommand(url: string): string {
  return (
    '[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072; ' +
    "& ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('" + url + "')))"
  );
}

/**
 * The short line for a product that vouches its script runs under `iex` (`#irm=1`: it turns TLS 1.2 on
 * itself and has no `param()` block). Needs PowerShell 3.0+ with TLS 1.2 available; older machines use
 * {@link bootCommand}.
 */
export function irmCommand(url: string): string {
  return 'irm ' + url + ' | iex';
}

/**
 * The commands the page shows for a product script: the main line, and for `irm=1` the
 * {@link bootCommand} fallback for old machines (Windows Server 2012 R2/2016 without TLS 1.2, PowerShell 2.0).
 * The page's inline `commands()` mirrors it (tested on the rendered page).
 */
export function productCommands(url: string, irm: boolean): { main: string; legacy: string } {
  const legacy = bootCommand(url);
  return irm ? { main: irmCommand(url), legacy } : { main: legacy, legacy: '' };
}

/**
 * The page for a product whose console has its own install script (it carries that product's
 * one-time token and setup): `code` is the dl-agent code the script enrols with (for the live
 * status), `boot` the script, `alt` an optional variant for SQL Server on another machine.
 */
export function productDeployUrl(
  origin: string,
  p: { code?: string; boot: string; alt?: string; product?: string; irm?: boolean },
): string {
  const q = new URLSearchParams();
  if (p.code) q.set('code', p.code);
  q.set('boot', p.boot);
  if (p.alt) q.set('alt', p.alt);
  if (p.product) q.set('p', p.product);
  if (p.irm) q.set('irm', '1');
  return `${origin}/deploy#${q.toString()}`;
}

/** The guided page: the code travels in the URL fragment (`#code=…`), never in a request line or a log. */
export function deployUrl(origin: string, code: string): string {
  return `${origin}/deploy#code=${encodeURIComponent(code)}`;
}

const STYLE = `
:root{color-scheme:light;--bg:#f6f7f9;--card:#fff;--fg:#1b2027;--muted:#5f6b78;--line:#dfe3e8;--accent:#2457c5;--accent-fg:#fff;--ok:#1a7f37;--ok-bg:#e8f5ec;--warn:#8a5a00;--warn-bg:#fff4dc;--bad:#b42318;--bad-bg:#fdecea;--code:#0f1720;--code-fg:#e8edf3}
@media (prefers-color-scheme:dark){:root:not([data-theme=light]){color-scheme:dark;--bg:#0e1116;--card:#161b22;--fg:#e6e9ee;--muted:#9aa5b1;--line:#2b323b;--accent:#6c9cff;--accent-fg:#0b1220;--ok:#4ac26b;--ok-bg:#12261a;--warn:#e3b341;--warn-bg:#2a2210;--bad:#ff7b72;--bad-bg:#2d1415;--code:#0a0d12;--code-fg:#e8edf3}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
main{max-width:720px;margin:0 auto;padding:32px 16px 48px}
header{margin-bottom:20px} h1{font-size:22px;margin:0 0 6px} .sub{color:var(--muted);margin:0}
.chip{display:inline-block;padding:2px 10px;border-radius:999px;background:var(--ok-bg);color:var(--ok);font-size:13px;font-weight:600;margin-bottom:10px}
.chip.neutral{background:var(--line);color:var(--fg)}
ol.steps{list-style:none;margin:0;padding:0;display:grid;gap:12px}
.step{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:16px 18px;display:grid;grid-template-columns:32px 1fr;gap:4px 12px}
.num{width:28px;height:28px;border-radius:50%;display:grid;place-items:center;font-weight:700;font-size:14px;background:var(--line);color:var(--fg)}
.step.done .num{background:var(--ok);color:#fff} .step.active .num{background:var(--accent);color:var(--accent-fg)}
.step h2{font-size:16px;margin:2px 0 4px} .step p{margin:0 0 8px;color:var(--muted)} .body{grid-column:2}
.cmd{display:flex;gap:8px;align-items:stretch;flex-wrap:wrap}
pre{flex:1 1 320px;margin:0;background:var(--code);color:var(--code-fg);border-radius:8px;padding:10px 12px;font:12.5px/1.5 ui-monospace,Consolas,monospace;white-space:pre-wrap;word-break:break-all;max-height:140px;overflow:auto}
button{font:inherit;font-weight:600;border:0;border-radius:8px;padding:10px 16px;background:var(--accent);color:var(--accent-fg);cursor:pointer;min-height:44px}
button:focus-visible,input:focus-visible,summary:focus-visible{outline:3px solid var(--accent);outline-offset:2px}
.status{display:flex;align-items:center;gap:10px;font-weight:600}
.spin{width:16px;height:16px;border-radius:50%;border:2px solid var(--line);border-top-color:var(--accent);animation:s 1s linear infinite}
@keyframes s{to{transform:rotate(360deg)}} @media (prefers-reduced-motion:reduce){.spin{animation:none}}
.mods{display:flex;flex-wrap:wrap;gap:6px;margin:10px 0 0;padding:0;list-style:none}
.mods li{padding:2px 10px;border-radius:999px;font-size:13px;background:var(--line)}
.mods li.ok{background:var(--ok-bg);color:var(--ok)} .mods li.warn{background:var(--warn-bg);color:var(--warn)} .mods li.bad{background:var(--bad-bg);color:var(--bad)}
.banner{border-radius:12px;padding:14px 16px;margin-top:16px;font-weight:600}
.banner.ok{background:var(--ok-bg);color:var(--ok)} .banner.bad{background:var(--bad-bg);color:var(--bad)} .banner.warn{background:var(--warn-bg);color:var(--warn)}
details{margin-top:20px;background:var(--card);border:1px solid var(--line);border-radius:12px;padding:12px 16px}
summary{cursor:pointer;font-weight:600} details ul{margin:8px 0 0;padding-left:20px;color:var(--muted)}
form{display:flex;gap:8px;flex-wrap:wrap;margin-top:8px} input{flex:1 1 200px;font:inherit;padding:10px 12px;border:1px solid var(--line);border-radius:8px;background:var(--card);color:var(--fg);min-height:44px}
details.legacy{margin-top:12px;padding:10px 14px} details.legacy .cmd{margin-top:8px}
.opt:not([hidden]){display:flex;gap:8px;align-items:center;margin-top:10px;cursor:pointer}.opt input{width:18px;height:18px;margin:0;flex:none}.small{font-size:13px;margin:8px 0 0}
@media (max-width:560px){pre{max-height:none}.cmd button{flex:1 1 100%}}
[hidden]{display:none!important} .muted{color:var(--muted)} kbd{font:12px ui-monospace,monospace;border:1px solid var(--line);border-radius:4px;padding:0 4px}`;

const BODY = `
<main>
  <header>
    <span class="chip neutral" id="product">DevLibrary Agent</span>
    <h1>Cài agent cho khách hàng</h1>
    <p class="sub" id="lead">Ba bước, khoảng 2 phút. Trang này tự cập nhật khi máy đã kết nối.</p>
  </header>

  <section id="nocode" hidden>
    <div class="banner warn">Liên kết này thiếu mã ghi danh.</div>
    <p class="muted">Mở lại nút "Triển khai agent" trong console sản phẩm, hoặc dán mã ghi danh (dạng XXXX-XXXX):</p>
    <form id="codeform"><label class="muted" for="codein" hidden>Mã ghi danh</label><input id="codein" autocomplete="off" placeholder="XXXX-XXXX" aria-label="Mã ghi danh"><button type="submit">Tiếp tục</button></form>
  </section>

  <ol class="steps" id="steps" hidden>
    <li class="step active" id="s1"><span class="num">1</span><div class="body">
      <h2>Mở PowerShell quyền Administrator</h2>
      <p>Trên máy chủ của khách sạn (máy có SQL Server): bấm <kbd>Start</kbd>, gõ <b>PowerShell</b>, chuột phải → <b>Run as administrator</b>.</p>
    </div></li>
    <li class="step" id="s2"><span class="num">2</span><div class="body">
      <h2>Dán lệnh và nhấn Enter</h2>
      <p id="expiry">Mã chỉ dùng một lần.</p>
      <div class="cmd"><pre id="cmd" aria-label="Lệnh cài đặt"></pre><button type="button" id="copy">Sao chép lệnh</button></div>
      <p class="muted small" id="source" hidden></p>
      <label class="muted opt" id="altrow" hidden><input type="checkbox" id="alton"> SQL Server nằm trên máy khác (không phải máy đang cài)</label>
      <details class="legacy" id="legacy" hidden><summary>Máy cũ? dùng lệnh này</summary>
        <p class="muted small">Windows Server 2012 R2/2016 chưa bật TLS 1.2, hoặc PowerShell 2.0: lệnh này tự bật TLS 1.2 rồi tải bằng WebClient.</p>
        <div class="cmd"><pre id="cmdold" aria-label="Lệnh cài đặt cho máy cũ"></pre><button type="button" id="copyold">Sao chép lệnh</button></div>
      </details>
    </div></li>
    <li class="step" id="s3"><span class="num">3</span><div class="body">
      <h2>Chờ máy kết nối</h2>
      <div class="status" role="status" aria-live="polite"><span class="spin" id="spin"></span><span id="state">Đang chờ máy chạy lệnh…</span></div>
      <ul class="mods" id="mods" hidden></ul>
      <p id="selftest" class="muted" hidden></p>
    </div></li>
  </ol>
  <div class="banner" id="result" hidden></div>

  <details id="help" hidden><summary>Gặp lỗi khi cài?</summary><ul>
    <li>Lệnh báo không có quyền: đóng PowerShell, mở lại bằng <b>Run as administrator</b>.</li>
    <li>Không tải được (proxy, tường lửa): máy cần ra Internet tới <b>agent.pms.id.vn</b> cổng 443.</li>
    <li>Phần mềm diệt virus chặn: cho phép thư mục <b>C:\\Program Files\\DevLibrary</b> rồi chạy lại lệnh.</li>
    <li>Chạy lại cùng lệnh là an toàn: agent đã cài được cập nhật, danh tính giữ nguyên.</li>
    <li>Máy đã có agent của sản phẩm khác: cứ chạy lệnh, sản phẩm này được thêm vào agent sẵn có.</li>
  </ul></details>
</main>`;

/** The page's script; `TEMPLATE` is the install line with {@link CODE_SLOT}, `LABELS` the product names. */
const SCRIPT = `
(function(){
  var $=function(id){return document.getElementById(id)};
  var CODE_RE=/^[A-Za-z0-9-]{4,64}$/;
  var STATE_TEXT={running:'đang chạy',starting:'đang khởi động',degraded:'có lỗi',stopped:'đã dừng',disabled:'tắt'};
  var STATE_CLASS={running:'ok',starting:'warn',degraded:'bad',stopped:'bad',disabled:''};
  var hp=new URLSearchParams(location.hash.slice(1));
  var code=hp.get('code');
  var safeBootUrl=${safeBootUrl.toString()};
  var boot=safeBootUrl(hp.get('boot'),SUFFIXES), alt=boot?safeBootUrl(hp.get('alt'),SUFFIXES):'';
  var badBoot=!!hp.get('boot')&&!boot;
  var irm=hp.get('irm')==='1';
  function bootLine(u){return ${JSON.stringify(bootCommand(BOOT_SLOT))}.split('${BOOT_SLOT}').join(u)}
  function irmLine(u){return ${JSON.stringify(irmCommand(BOOT_SLOT))}.split('${BOOT_SLOT}').join(u)}
  // Same result as productCommands(u, irm).
  function commands(u){return irm?{main:irmLine(u),legacy:bootLine(u)}:{main:bootLine(u),legacy:''}}
  var timer=null, started=Date.now();
  function show(id,on){$(id).hidden=!on}
  function step(n,cls){$('s'+n).className='step '+cls}
  function banner(cls,text){var b=$('result');b.className='banner '+cls;b.textContent=text;b.hidden=false}
  function start(c){
    code=c.trim().toUpperCase(); show('nocode',false); show('steps',true); show('help',true);
    var prod=hp.get('p'); if(prod&&LABELS.hasOwnProperty(prod)){var pc=$('product');pc.textContent=LABELS[prod];pc.className='chip'}
    if(badBoot){show('steps',false);show('help',false);banner('bad','Liên kết cài đặt không hợp lệ. Mở lại nút "Triển khai agent" trong console sản phẩm.');return}
    if(boot){
      $('source').textContent='Lệnh tải bộ cài từ '+new URL(boot).host+' (console sản phẩm).';show('source',true);
      var pick=function(){var c=commands($('alton').checked&&alt?alt:boot);$('cmd').textContent=c.main;$('cmdold').textContent=c.legacy;show('legacy',!!c.legacy)};
      if(alt){show('altrow',true);$('alton').addEventListener('change',pick)}
      pick();
    } else { $('cmd').textContent=TEMPLATE.split('${CODE_SLOT}').join(code) }
    if(code){poll();return}
    // A product that mints its hub code during the install (Check-In) sends no code: the console follows the agent.
    $('lead').textContent='Ba bước, khoảng 2 phút.';
    $('spin').hidden=true; $('s3').querySelector('h2').textContent='Xem kết quả trong console';
    $('state').textContent='Lệnh chạy xong (báo "Hoàn tất") thì quay lại console sản phẩm: agent hiện là đã kết nối.';
  }
  function copyFrom(pre,btn){
    $(btn).addEventListener('click',function(){
      var t=$(pre).textContent, b=$(btn);
      var done=function(){b.textContent='Đã sao chép ✓';step(1,'done');step(2,'active');setTimeout(function(){b.textContent='Sao chép lệnh'},2500)};
      if(navigator.clipboard&&window.isSecureContext){navigator.clipboard.writeText(t).then(done,function(){selectCmd(pre)})}else{selectCmd(pre)}
    });
  }
  copyFrom('cmd','copy'); copyFrom('cmdold','copyold');
  function selectCmd(id){var r=document.createRange();r.selectNodeContents($(id));var s=getSelection();s.removeAllRanges();s.addRange(r)}
  $('codeform').addEventListener('submit',function(e){e.preventDefault();var v=$('codein').value.trim();if(CODE_RE.test(v)){location.hash='code='+encodeURIComponent(v.toUpperCase());start(v)}else{$('codein').setAttribute('aria-invalid','true')}});
  function fmtTime(ts){return new Date(ts*1000).toLocaleString('vi-VN',{hour:'2-digit',minute:'2-digit',day:'2-digit',month:'2-digit'})}
  function render(s){
    if(s.product){var p=$('product');p.textContent=LABELS[s.product]||s.product;p.className='chip'}
    if(s.expires_at&&s.state==='waiting'){$('expiry').textContent='Mã chỉ dùng một lần, hết hạn lúc '+fmtTime(s.expires_at)+'.'}
    if(s.state==='unknown'){stop();banner('bad','Mã ghi danh không đúng. Kiểm tra lại liên kết hoặc tạo mã mới trong console sản phẩm.');return}
    if(s.state==='expired'){stop();banner('bad','Mã đã hết hạn mà chưa được dùng. Tạo mã mới trong console sản phẩm rồi mở lại trang.');return}
    if(s.state==='revoked'){stop();banner('bad','Mã hoặc agent đã bị thu hồi. Liên hệ quản trị DevLibrary.');return}
    if(s.state==='waiting'){if(Date.now()-started>20*60*1000){stop();$('state').textContent='Chưa thấy máy kết nối. Tải lại trang sau khi chạy lệnh.'}return}
    var a=s.agent; step(1,'done'); step(2,'done');
    $('state').textContent=(a.online?'Đã kết nối: ':'Đã ghi danh, đang chờ tín hiệu: ')+a.hostname+' (phiên bản '+a.version+')';
    var mods=$('mods'); mods.innerHTML='';
    a.modules.forEach(function(m){var li=document.createElement('li');li.className=STATE_CLASS[m.state]||'';li.textContent=(LABELS[m.name]||m.name)+': '+(STATE_TEXT[m.state]||m.state);mods.appendChild(li)});
    mods.hidden=a.modules.length===0;
    var t=a.selftest;
    if(t){$('selftest').textContent='Tự kiểm tra: '+t.pass+' đạt · '+t.warn+' cảnh báo · '+t.fail+' lỗi';show('selftest',true)}
    if(a.online&&t){
      stop(); step(3,'done'); $('spin').hidden=true;
      if(t.fail>0){banner('warn','Agent đã chạy nhưng tự kiểm tra còn lỗi. Gửi màn hình này cho kỹ thuật để xử lý.')}
      else{banner('ok','Hoàn tất. Agent đã chạy trên '+a.hostname+'. Có thể đóng trang này.')}
    } else { step(3,'active') }
  }
  function stop(){if(timer){clearTimeout(timer);timer=null}$('spin').hidden=true}
  function poll(){
    fetch('/deploy/status',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({code:code}),cache:'no-store'})
      .then(function(r){return r.json()})
      .then(function(s){render(s);if(s.state==='waiting'||(s.state==='connected'&&!(s.agent.online&&s.agent.selftest))){if($('spin').hidden===false||s.state==='connected'){timer=setTimeout(poll,s.state==='waiting'?5000:8000)}}})
      .catch(function(){timer=setTimeout(poll,10000)});
  }
  if(code&&CODE_RE.test(code)){start(code)}else if(!code&&hp.get('boot')){start('')}else{show('nocode',true)}
})();`;

/** The whole page; `nonce` authorises its one inline script under the response's CSP. */
export function renderDeployPage(origin: string, nonce: string): string {
  const template = JSON.stringify(installCommand(origin, CODE_SLOT));
  return `<!doctype html>
<html lang="vi"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="referrer" content="no-referrer"><title>Cài DevLibrary Agent</title>
<style>${STYLE}</style></head><body>${BODY}
<script nonce="${nonce}">var TEMPLATE=${template};var LABELS=${JSON.stringify(PRODUCT_LABELS)};var SUFFIXES=${JSON.stringify(BOOT_HOST_SUFFIXES)};${SCRIPT}</script>
</body></html>`;
}
