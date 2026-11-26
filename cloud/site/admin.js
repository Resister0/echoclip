// echoclip.link/admin (chat 28: moved out of admin.html - the page's Content-Security-Policy only runs script files)
const $ = (s) => document.querySelector(s);
const esc = (t) => String(t ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const gb = (b) => { b = b || 0; return b >= 1e12 ? (b / 1e12).toFixed(2) + " TB" : b >= 1e9 ? (b / 1e9).toFixed(b >= 100e9 ? 0 : 1) + " GB" : (b / 1e6).toFixed(0) + " MB"; };
// time zone for every clock time and date on the page (26 Nov 2026): Denmark (CET / CEST by itself), US Eastern (EST / EDT
// by itself) or UTC, picked top right and remembered in this browser. Day totals stay UTC days - the server counts per UTC
// day - so only the "day starts at" text follows the choice.
const TZS = [["Europe/Copenhagen", "Denmark"], ["America/New_York", "US Eastern"], ["UTC", "UTC"]];
let TZ = (() => { let s = ""; try { s = localStorage.getItem("sonela_admin_tz") || ""; } catch { } return TZS.some((z) => z[0] === s) ? s : TZS[0][0]; })();
// "CET" / "CEST" need en-GB, "EST" / "EDT" need en-US (each spells the other's zone as "GMT+1")
const tzAbbr = (d) => new Intl.DateTimeFormat(TZ.startsWith("America/") ? "en-US" : "en-GB", { timeZone: TZ, timeZoneName: "short" }).formatToParts(d).find((x) => x.type === "timeZoneName").value;
const when = (t) => { const d = new Date(t * 1000); return d.toLocaleString(undefined, { timeZone: TZ, day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit" }) + " " + tzAbbr(d); };
const dayStart = () => {
  if (TZ === "UTC") return "UTC day";
  const d = new Date(new Date().toISOString().slice(0, 10) + "T00:00:00Z");
  const hm = d.toLocaleTimeString("en-GB", { timeZone: TZ, hour: "numeric", minute: "2-digit" });
  const before = d.toLocaleDateString("en-CA", { timeZone: TZ }) < d.toISOString().slice(0, 10);
  return `UTC day, starts at ${hm} ${tzAbbr(d)}${before ? " the evening before" : ""}`;
};
const ago = (t) => { if (!t) return "-"; const d = Date.now() / 1000 - t; return `<span title="${esc(when(t))}">${d < 3600 ? Math.round(d / 60) + " min ago" : d < 86400 ? Math.round(d / 3600) + " h ago" : Math.round(d / 86400) + " days ago"}</span>`; };
const day = (t) => t ? new Date(t * 1000).toLocaleDateString(undefined, { timeZone: TZ, day: "numeric", month: "short", year: "numeric" }) : "-";
const money = (v, c) => (v || 0).toLocaleString(undefined, { style: "currency", currency: c || "EUR" });
// signed in with the email code (the admin address only, chat 30): a session token like the app's, kept in this browser until
// "Sign out" (ends the session on the server too) or 180 days unused. The old ADMIN_KEY is no longer stored anywhere.
// sonela.app/admin (21 Nov 2026): this same page, served by the Sonela hub with an app switcher in the top-left corner.
// The hub puts its settings on <main id="app" data-cfg='{...}'>: api = where Echo's own calls go (the hub passes them on
// to echoclip.link), site = the address for share-link / profile / mod links, apps = [{id, name, note, icon, stats}] for
// the switcher (stats = that app's numbers in this page's shape). Without data-cfg (echoclip.link/admin) nothing changes.
const CFG = (() => { try { return JSON.parse(document.getElementById("app").dataset.cfg || "{}"); } catch { return {}; } })();
const API = CFG.api || "", SITE = CFG.site || "";
const APPS = Array.isArray(CFG.apps) ? CFG.apps.filter((a) => a && a.id && a.name) : [];
let APP = (() => {
  const ids = APPS.map((a) => a.id), q = new URLSearchParams(location.search).get("app");
  let s = ""; try { s = localStorage.getItem("sonela_admin_app") || ""; } catch { }
  return ids.includes(q) ? q : ids.includes(s) ? s : ids[0] || "";
})();
const curApp = () => APPS.find((a) => a.id === APP) || null;
const appName = () => (curApp() || { name: "Echo" }).name;
const statsUrl = () => (curApp() && curApp().stats) || API + "/admin/stats";
// the switcher (like an account switcher): the app's icon + name in the title; click = the list of apps, pick one = the
// whole page shows that app (remembered in this browser and in the address, ?app=)
function switcher() {
  if (!APPS.length) return `<h1>Echo stats</h1>`;
  const a = curApp(), icon = (x) => x.icon ? `<img src="${esc(x.icon)}" alt="" width="28" height="28">` : `<span class="swn">${esc(x.name[0])}</span>`;
  const item = (x) => `<button class="swi" role="menuitemradio" aria-checked="${x.id === APP}" data-app="${esc(x.id)}">${icon(x)}
    <span class="swt"><b>${esc(x.name)}</b>${x.note ? `<span class="sub">${esc(x.note)}</span>` : ""}</span><span class="swk" aria-hidden="true">${x.id === APP ? "✓" : ""}</span></button>`;
  return `<div class="sw"><button id="swb" aria-haspopup="menu" aria-expanded="false" aria-controls="swm" title="Switch app">${icon(a)}<h1>${esc(a.name)} stats</h1><span class="swc" aria-hidden="true">▾</span></button>
    <div class="swm" id="swm" role="menu" aria-label="Sonela apps" hidden><div class="swh">Sonela apps</div>${APPS.map(item).join("")}</div></div>`;
}
function switchTo(id) {
  if (id === APP) return swOpen(false);
  APP = id;
  try { localStorage.setItem("sonela_admin_app", id); } catch { }
  history.replaceState(null, "", "?app=" + encodeURIComponent(id) + location.hash);
  const h = document.querySelector(".sw h1"); if (h) h.textContent = appName() + " stats…";
  swOpen(false); load(true);
}
function swOpen(on) {
  const m = $("#swm"), b = $("#swb"); if (!m || !b) return;
  m.hidden = !on; b.setAttribute("aria-expanded", String(on));
  if (on) { const c = m.querySelector('[aria-checked="true"]') || m.querySelector(".swi"); if (c) c.focus(); }
}
if (APPS.length) {
  document.addEventListener("click", (e) => {
    const t = e.target.closest && e.target;
    if (t && t.closest("#swb")) return swOpen($("#swm").hidden);
    const it = t && t.closest(".swi"); if (it) return switchTo(it.dataset.app);
    if (!(t && t.closest("#swm"))) swOpen(false);
  });
  document.addEventListener("keydown", (e) => {
    const m = $("#swm"); if (!m || m.hidden) return;
    const items = [...m.querySelectorAll(".swi")], i = items.indexOf(document.activeElement);
    if (e.key === "Escape") { swOpen(false); $("#swb").focus(); e.stopPropagation(); }
    else if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); const n = items[(i + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length]; if (n) n.focus(); }
  }, true);
  const st = document.createElement("style");
  st.textContent = `.sw{position:relative}#swb{display:flex;align-items:center;gap:10px;background:none;border:1px solid transparent;padding:4px 10px 4px 4px;margin-left:-5px;border-radius:12px}
    #swb:hover,#swb[aria-expanded="true"]{border-color:var(--line);background:var(--card)}#swb img,.swi img,.swn{width:28px;height:28px;border-radius:8px;flex:none;object-fit:contain}
    .swn{display:grid;place-items:center;background:var(--accent);color:#fff;font-weight:700}.swc{color:var(--muted);font-size:12px}
    .swm{position:absolute;left:-5px;top:calc(100% + 6px);z-index:30;min-width:250px;background:var(--card);border:1px solid var(--line);border-radius:12px;padding:6px;box-shadow:0 10px 30px rgba(0,0,0,.3)}
    .swh{color:var(--muted);font-size:11.5px;font-weight:600;text-transform:uppercase;letter-spacing:.06em;padding:6px 8px 4px}
    .swi{display:flex;align-items:center;gap:10px;width:100%;text-align:left;border:0;background:none;padding:8px;border-radius:8px}
    .swi:hover,.swi:focus-visible{background:color-mix(in srgb,var(--text) 7%,transparent);outline:none}.swi[aria-checked="true"]{background:color-mix(in srgb,var(--accent) 14%,transparent)}
    .swt{display:flex;flex-direction:column;flex:1;min-width:0}.swt .sub{font-size:12px}.swk{color:var(--accent);font-weight:700}`;
  document.head.appendChild(st);
}
let key = ""; try { localStorage.removeItem("echo_admin_key"); sessionStorage.removeItem("echo_admin_key"); key = localStorage.getItem("echo_admin_token") || ""; } catch { }
const auth = (extra) => ({ authorization: "Bearer " + key, ...(extra || {}) });

function login(msg) {
  $("#app").innerHTML = `<div class="login"><h1>${APPS.length ? "Sonela admin" : "Echo stats"}</h1><div class="sub">Sign in with your email - we'll send you a code.</div>
    <input id="em" type="email" placeholder="Email" autocomplete="email"><button id="go">Send code</button>
    <div id="step2" style="display:none"><input id="cd" inputmode="numeric" maxlength="6" placeholder="6-digit code" autocomplete="one-time-code"><button id="ok">Sign in</button></div>
    <div class="err" id="er">${msg ? esc(msg) : ""}</div></div>`;
  const post = async (path, b) => { const r = await fetch(API + path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(b) }); return [r, await r.json().catch(() => ({}))]; };
  $("#go").onclick = async () => {
    const email = $("#em").value.trim(); if (!email) return;
    $("#go").disabled = true; $("#er").textContent = "";
    try { const [r, j] = await post("/auth/start", { email, admin: true }); if (!r.ok) throw new Error(j.error || "Error " + r.status); } catch (e) { $("#er").textContent = e.message; $("#go").disabled = false; return; }
    $("#step2").style.display = ""; $("#er").textContent = "Check your email for the code."; $("#cd").focus();
  };
  $("#ok").onclick = async () => {
    try {
      const [r, j] = await post("/auth/verify", { email: $("#em").value.trim(), code: $("#cd").value, admin: true });
      if (!r.ok || !j.token) throw new Error(j.error || "Error " + r.status);
      key = j.token; try { localStorage.setItem("echo_admin_token", key); } catch { } load();
    } catch (e) { $("#er").textContent = e.message; }
  };
  $("#em").onkeydown = (e) => { if (e.key === "Enter") $("#go").click(); };
  $("#cd").onkeydown = (e) => { if (e.key === "Enter") $("#ok").click(); };
  $("#em").focus();
}

async function load(manual) {
  if (!key) return login();
  const re = $("#re"); if (re && manual) { re.disabled = true; re.textContent = "Refreshing…"; }
  const y = window.scrollY;
  let r;
  try { r = await fetch(statsUrl(), { headers: auth() }); } catch { return login("Couldn't reach echoclip.link"); }
  if (r.status === 403) { const j = await r.json().catch(() => ({})); try { localStorage.removeItem("echo_admin_token"); } catch { } key = ""; return login((j.error || "Signed out - sign in again")); }
  if (!r.ok) return login("Error " + r.status);
  draw(await r.json());
  window.scrollTo(0, y); // a refresh keeps your place (tab, filters and scroll)
}

function tile(k, v, s, cls) { return `<div class="tile ${cls || ""}"><div class="k">${k}</div><div class="v">${v}</div>${s ? `<div class="s">${s}</div>` : ""}</div>`; }

// ----- charts (18 Nov 2026): HTML/CSS columns + bars so text stays sharp at any width. A y axis with clean ticks and
// gridlines, dates under the columns, the newest value printed on its column, a legend for 2+ series, and one tooltip
// with every series of the hovered day. Colours = .s1..s5 (admin.html, checked for colour-blind safety in both themes).
const CH = {}; // chart id -> {labels, series} for the tooltip
let chN = 0;
const nf = (v) => (Math.round(v * 100) / 100).toLocaleString();
// clean axis top + step: 1 / 2 / 2.5 / 5 x 10^n, about 4 gridlines
function niceAxis(max) {
  if (!(max > 0)) return { top: 4, step: 1 };
  const raw = max / 4, p = Math.pow(10, Math.floor(Math.log10(raw)));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * p).find((s) => s >= raw);
  const st = step < 1 && max >= 4 ? 1 : step;
  return { top: Math.ceil(max / st) * st, step: st };
}
const shortDay = (d) => new Date(d + "T12:00:00Z").toLocaleDateString(undefined, { day: "numeric", month: "short" });
// colChart: labels = day strings (oldest .. newest), series = [{name, vals, c: 1..5}], stacked columns
function colChart({ title, sub, labels, series, h = 170, unit = "" }) {
  const id = "ch" + (++chN);
  const tot = labels.map((_, i) => series.reduce((a, s) => a + (s.vals[i] || 0), 0));
  const { top, step } = niceAxis(Math.max(...tot));
  CH[id] = { labels, series, unit };
  const ticks = []; for (let v = 0; v <= top + 1e-9; v += step) ticks.push(v);
  const n = labels.length, every = n > 20 ? 7 : n > 10 ? 3 : 1;
  const cols = labels.map((d, i) => {
    const segs = series.map((s) => s.vals[i] || 0 ? `<i class="s${s.c}" style="height:${(s.vals[i] / top * 100).toFixed(2)}%"></i>` : "").reverse().join("");
    const last = i === n - 1 && tot[i] > 0 ? `<b class="cc-val" style="bottom:${(tot[i] / top * 100).toFixed(2)}%">${nf(tot[i])}${unit}</b>` : "";
    return `<div class="cc-col" data-ch="${id}" data-i="${i}" tabindex="0" aria-label="${esc(shortDay(d))}: ${nf(tot[i])}${unit}"><div class="cc-stk">${segs}</div>${last}</div>`;
  }).join("");
  const xl = labels.map((d, i) => `<span style="left:${((i + .5) / n * 100).toFixed(2)}%">${(n - 1 - i) % every === 0 ? (i === n - 1 ? "Today" : esc(shortDay(d))) : ""}</span>`).join("");
  const grid = ticks.map((v) => `<div class="cc-g" style="bottom:${(v / top * 100).toFixed(2)}%"><span>${nf(v)}</span></div>`).join("");
  const legend = series.length > 1 ? `<div class="cc-lg">${series.map((s) => `<span><i class="s${s.c}"></i>${esc(s.name)}</span>`).join("")}</div>` : "";
  return `<div class="card cc"><div class="cc-h"><div><div class="cc-t">${esc(title)}</div>${sub ? `<div class="sub">${sub}</div>` : ""}</div>${legend}</div>
    <div class="cc-plot" style="height:${h}px">${grid}<div class="cc-cols">${cols}</div></div><div class="cc-x">${xl}</div></div>`;
}
// hbars: one bar per row, sorted by value, value at the bar's tip (e.g. CPU per graphics card)
function hbars({ title, sub, rows, unit = "", max }) {
  if (!rows.length) return "";
  const m = max || Math.max(...rows.map((r) => r.v)) || 1;
  return `<div class="card hb"><div class="cc-t">${esc(title)}</div>${sub ? `<div class="sub">${sub}</div>` : ""}
    ${rows.map((r) => `<div class="hb-r" title="${esc(r.k)}: ${nf(r.v)}${unit}${r.note ? " · " + esc(r.note) : ""}"><div class="hb-k"><div class="hb-n">${esc(r.k)}</div>${r.note ? `<div class="sub">${esc(r.note)}</div>` : ""}</div>
      <div class="hb-t"><i class="s${r.c || 1}" style="width:${Math.max(.5, r.v / m * 100).toFixed(2)}%"></i><b>${nf(r.v)}${unit}</b></div></div>`).join("")}</div>`;
}
// sparkline for a stat tile: the series in a muted line, the newest point in the accent
function spark(vals) {
  if (!vals || vals.length < 2) return "";
  const mx = Math.max(1, ...vals), W = 100, H = 28, x = (i) => (i / (vals.length - 1) * W).toFixed(2), y = (v) => (H - 2 - v / mx * (H - 4)).toFixed(2);
  const d = vals.map((v, i) => `${i ? "L" : "M"}${x(i)} ${y(v)}`).join("");
  return `<svg class="spark" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" aria-hidden="true"><path d="${d}"/></svg>`;
}
// change vs the days before: full days only (21 Nov 2026 review: today isn't over, so "this week" with today in it read
// "▼" every morning). 7 full days vs the 7 before when there's enough data (14 days of data: 6 vs 6). Up = good unless told otherwise.
function delta(vals, upGood = true) {
  if (!vals || vals.length < 13) return "";
  const n = Math.min(7, Math.floor((vals.length - 1) / 2));
  const a = vals.slice(-1 - n, -1).reduce((x, y) => x + y, 0), b = vals.slice(-1 - 2 * n, -1 - n).reduce((x, y) => x + y, 0);
  // 18 Nov 2026: the real difference ("+40 vs last week"), not a percentage - "▲ 1000%" from 4 to 44 said nothing
  const dlt = a - b, span = n === 7 ? "the week before" : `the ${n} days before`;
  if (!dlt) return `<span class="dl">same as ${span}</span>`;
  return `<span class="dl ${(dlt > 0) === upGood ? "up" : "down"}" title="the ${n} full days before today: ${a} · the ${n} before those: ${b} (daily numbers added up)">${dlt > 0 ? "▲ +" : "▼ −"}${Math.abs(dlt).toLocaleString()} vs ${span}</span>`;
}
// stat tile with an optional sparkline + delta
function stat(k, v, s, vals, upGood) {
  // the line ends yesterday: today's half-finished day would always look like a drop
  return `<div class="tile"><div class="k">${k}</div><div class="v">${v}</div>${vals ? delta(vals, upGood) : ""}${s ? `<div class="s">${s}</div>` : ""}${spark(vals && vals.slice(0, -1))}</div>`;
}
// one tooltip for every chart: the day + each series (value first), total when stacked
function chartTips() {
  let tip = $("#cctip"); if (!tip) { tip = document.createElement("div"); tip.id = "cctip"; tip.hidden = true; document.body.appendChild(tip); }
  const show = (col, x, y) => {
    const c = CH[col.dataset.ch], i = +col.dataset.i; if (!c) return;
    tip.replaceChildren();
    const h = document.createElement("div"); h.className = "tt-h"; h.textContent = shortDay(c.labels[i]) + (i === c.labels.length - 1 ? " (today)" : ""); tip.appendChild(h);
    let t = 0;
    for (const s of c.series) { const v = s.vals[i] || 0; t += v;
      const r = document.createElement("div"); r.className = "tt-r";
      const key = document.createElement("i"); key.className = "s" + s.c; const b = document.createElement("b"); b.textContent = nf(v) + c.unit;
      const nm = document.createElement("span"); nm.textContent = s.name; r.append(key, b, nm); tip.appendChild(r); }
    if (c.series.length > 1) { const r = document.createElement("div"); r.className = "tt-r tt-t"; const b = document.createElement("b"); b.textContent = nf(t) + c.unit; const nm = document.createElement("span"); nm.textContent = "in all"; r.append(document.createElement("i"), b, nm); tip.appendChild(r); }
    tip.hidden = false;
    const r = tip.getBoundingClientRect(), px = Math.min(window.innerWidth - r.width - 8, x + 14), py = Math.max(8, y - r.height - 12);
    tip.style.left = px + "px"; tip.style.top = py + "px";
  };
  document.addEventListener("pointermove", (e) => { const col = e.target.closest && e.target.closest(".cc-col"); if (col) show(col, e.clientX, e.clientY); else tip.hidden = true; });
  document.addEventListener("focusin", (e) => { const col = e.target.closest && e.target.closest(".cc-col"); if (col) { const r = col.getBoundingClientRect(); show(col, r.right, r.top + 40); } });
  document.addEventListener("focusout", () => { tip.hidden = true; });
}
chartTips();

// ----- Overview: what needs a look (18 Nov 2026) - everything here comes from the same /admin/stats answer; open reports
// arrive a moment later (drawMods). Each line links to the tab with the details. Status = icon + word, never colour alone.
function attention(j, A) {
  // 18 Nov 2026 (the user): only things worth acting on - no FYIs - and each one can be dismissed (×). A dismissed line
  // comes back only when its number grows (kept in this browser).
  const items = [], add = (id, n, lvl, text, tab) => items.push({ id, n, lvl, text, tab });
  const K = (j.site && j.site.keys) || {};
  const cr = {}; for (const x of Object.keys(K)) { const m = x.match(/^crash:[a-z]+:(.+)$/); if (m && K[x][1]) cr[m[1]] = (cr[m[1]] || 0) + K[x][1]; }
  for (const v of Object.keys(cr).sort((a, b) => vcmp(b, a))) add("crash:" + v, cr[v], "serious", `${cr[v]} sent log${cr[v] === 1 ? "" : "s"} with an Echo crash this week on ${v}`, "apps");
  for (const r of j.releases || []) if (r.paused) add("paused:" + r.version, 1, "warning", `Updates to ${r.version} are paused - nobody gets it as an update`, "apps");
  if ((j.over || []).length) add("over", j.over.length, "warning", `${j.over.length} Echo Cloud account${j.over.length === 1 ? " is" : "s are"} over the plan - clips go after 30 days`, "cloud");
  // soft_fixable (26 Nov 2026): leaves out cards that have no video encoder at all (server NO_ENCODER_GPUS); older server: all
  const pc = j.perf_cpu, soft = pc && (pc.soft_fixable != null ? pc.soft_fixable : (pc.enc || []).filter((r) => /x264/i.test(r.k || "")).reduce((a, r) => a + r.pcs, 0));
  if (soft) add("x264", soft, "serious", `Echo had to use the software encoder on ${soft} day${soft === 1 ? "" : "s"} of PC use in the last 30 days - no graphics encoder worked there`, "perf");
  const gone = attDismissed(), shown = items.filter((x) => !(gone[x.id] >= x.n));
  const ICON = { serious: "▲", warning: "!" }, WORD = { serious: "Look at", warning: "Heads up" };
  const li = (x) => `<div class="att ${x.lvl}" data-id="${esc(x.id)}" data-n="${x.n}"><span class="att-i" aria-hidden="true">${ICON[x.lvl]}</span><span class="att-w">${WORD[x.lvl]}</span><a class="att-t" href="#${x.tab}">${esc(x.text)}</a><button class="att-x" title="Dismiss - it comes back if this number grows" aria-label="Dismiss">×</button></div>`;
  return `<h2>Needs attention</h2><div class="card atts" id="atts">
    <div class="att serious" id="attmod" hidden><span class="att-i" aria-hidden="true">▲</span><span class="att-w">Look at</span><a class="att-t" href="#mod"></a></div>
    ${shown.map(li).join("")}
    <div class="att good" id="attok"${shown.length ? " hidden" : ""}><span class="att-i" aria-hidden="true">✓</span><span class="att-w">All good</span><span class="att-t">nothing to look at</span></div></div>`;
}
const attDismissed = () => { try { return JSON.parse(localStorage.getItem("echo_admin_dismissed") || "{}"); } catch { return {}; } };
document.addEventListener("click", (e) => {
  const x = e.target.closest && e.target.closest(".att-x"); if (!x) return;
  const row = x.closest(".att"), d = attDismissed(); d[row.dataset.id] = +row.dataset.n;
  try { localStorage.setItem("echo_admin_dismissed", JSON.stringify(d)); } catch { }
  row.remove();
  const box = $("#atts"); if (box && !box.querySelector(".att:not(.good):not([hidden])")) { const ok = $("#attok"); if (ok) ok.hidden = false; }
});
// ----- Overview: the last 7 days from a website visit to a running Echo
function funnel(j, I, vis, dls) {
  if (j.lite) {
    const A = j.apps && appDays(j.apps), avg = A ? A.days.slice(6, 13).reduce((t, d) => t + A.per[d].win + A.per[d].lin, 0) / 7 : null;
    const steps = [["Downloads", dls[1], "download clicks, added up"], ["New installs", I.d7, ""], ["Apps per day", avg == null ? null : Math.round(avg * 10) / 10, "average of the 7 days before today"]];
    return `<h2>Last 7 days</h2><div class="funnel">${steps.map(([l, v, note], i) => { const prev = i ? steps[i - 1][1] : null;
      const conv = note ? `<span class="fn-c">${note}</span>` : prev && v != null ? `<span class="fn-c">${Math.round(v / prev * 100)}% of the step before</span>` : `<span class="fn-c">&nbsp;</span>`;
      return `${i ? '<span class="fn-a" aria-hidden="true">→</span>' : ""}<div class="fn-s"><div class="k">${l}</div><div class="v">${v == null ? "-" : v.toLocaleString()}</div>${conv}</div>`; }).join("")}</div>`;
  }
  // different PCs are only counted since that started (18 Nov 2026): until it covers the whole week it reads too low
  const U = j.apps && j.apps.uniq, full = U && U.since && U.since <= new Date(Date.now() - 6 * 864e5).toISOString().slice(0, 10);
  const u = full ? U["7"] : null;
  const steps = [["Website visits", vis[1]], ["Downloads from the site", dls[1]], ["New installs", I.d7], [`PCs with ${appName()}`, u ? u.total : null]];
  return `<h2>Last 7 days</h2><div class="funnel">${steps.map(([l, v], i) => { const prev = i ? steps[i - 1][1] : null;
    // installs also come from GitHub / winget / AppImageHub (not through the site), so no "%" under them
    const conv = i === 3 ? `<span class="fn-c">${u ? "different PCs, new and old (told apart by internet address)" : `counted from ${esc(U && U.since ? shortDay(U.since) : "now")} - full after a week`}</span>`
      : i === 2 ? `<span class="fn-c">from every source: the site, GitHub, winget…</span>`
      : i === 1 && prev && v != null ? `<span class="fn-c">about ${Math.round(v / prev * 100)}% of the visits</span>` : i ? `<span class="fn-c">&nbsp;</span>` : `<span class="fn-c">a person counts once a day</span>`;
    return `${i ? '<span class="fn-a" aria-hidden="true">→</span>' : ""}<div class="fn-s"><div class="k">${l}</div><div class="v">${v == null ? "-" : v.toLocaleString()}</div>${conv}</div>`; }).join("")}</div>
`;
}
// ----- "updated 3 min ago" that keeps itself current; a quiet refresh every 5 minutes while the page is in front
function tickUpdated() {
  const u = $("#upd"); if (!u) return;
  const s = Math.max(0, Date.now() / 1000 - +u.dataset.at);
  u.textContent = s < 60 ? "updated just now" : s < 3600 ? `updated ${Math.floor(s / 60)} min ago` : `updated ${Math.floor(s / 3600)} h ago`;
}
setInterval(() => {
  tickUpdated();
  const u = $("#upd"), typing = document.activeElement && /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName);
  if (u && document.visibilityState === "visible" && !typing && Date.now() / 1000 - +u.dataset.at > 300) load();
}, 30000);
// ----- keys: 1-6 = tabs, R = refresh (not while typing)
document.addEventListener("keydown", (e) => {
  if (e.ctrlKey || e.metaKey || e.altKey || /^(INPUT|TEXTAREA|SELECT)$/.test((e.target && e.target.tagName) || "")) return;
  const n = parseInt(e.key, 10);
  if (n >= 1 && n <= VIS.length) location.hash = VIS[n - 1][0];
  else if (e.key === "r" || e.key === "R") { if ($("#re")) load(true); }
});
// ----- every table sorts by clicking a column header (again = the other way). Totals and "nothing here" rows stay put.
const sortKey = (td) => {
  const t = ((td && td.childNodes[0] && td.childNodes[0].textContent) || (td && td.textContent) || "").trim();
  const ago = t.match(/^(\d+) (min|h|days?) ago$/); if (ago) return +ago[1] * ({ min: 1, h: 60, day: 1440, days: 1440 })[ago[2]];
  const sz = t.match(/^([\d.,]+) (MB|GB|TB)$/); if (sz) return parseFloat(sz[1].replace(/,/g, "")) * ({ MB: 1, GB: 1e3, TB: 1e6 })[sz[2]];
  if (/^[-+€$]?[\d.,]+ ?(%|h|min| MB)?$/.test(t)) return parseFloat(t.replace(/[^\d.-]/g, ""));
  const d = Date.parse(t); if (/[a-z]{3} \d{1,2}, \d{4}/i.test(t) && !isNaN(d)) return d;
  return t.toLowerCase();
};
document.addEventListener("click", (e) => {
  const th = e.target.closest && e.target.closest("th"); if (!th) return;
  const table = th.closest("table"); if (!table || table.id === "rels" || table.querySelector("input,textarea")) return;
  const head = th.parentElement, col = [...head.children].indexOf(th);
  const rows = [...table.querySelectorAll("tr")].filter((r) => r !== head && r.children.length === head.children.length && !r.querySelector("td[colspan]") && !/^(All versions|In all)$/.test((r.children[0].textContent || "").trim()));
  if (rows.length < 2) return;
  const dir = th.dataset.sort === "desc" ? "asc" : "desc";
  head.querySelectorAll("th").forEach((x) => delete x.dataset.sort); th.dataset.sort = dir;
  rows.sort((a, b) => { const x = sortKey(a.children[col]), y = sortKey(b.children[col]);
    const c = typeof x === "number" && typeof y === "number" ? x - y : String(x).localeCompare(String(y), undefined, { numeric: true });
    return dir === "asc" ? c : -c; });
  const box = rows[0].parentElement, rest = [...box.children].filter((r) => r !== head && !rows.includes(r));
  rows.forEach((r) => box.appendChild(r)); rest.forEach((r) => box.appendChild(r)); // totals stay at the bottom
});

// ----- website (counted without cookies; days are UTC) -----
const CLICK_NAMES = { "click:plans": "See plans / Pricing", "click:storage": "Storage (menu)", "click:features": "Features (menu)",
  "click:download": "Download (jump to buttons)", "click:discord": "Discord", "click:changelog": "Changelog", "click:github": "GitHub",
  "dl:windows": "Download: Windows installer", "dl:windows-portable": "Download: Windows portable zip (no install)", "dl:linux": "Download: Linux AppImage", "dl:linux-tar": "Download: Linux tar.gz", "dl:flatpak": "Download: Linux Flatpak" };
// crashes: logs people sent with "Send log", and how many of them contain an Echo crash (last 30 days)
function crashes(k) {
  const vers = {};
  for (const x of Object.keys(k)) {
    const m = x.match(/^(logs|crash):([a-z]+):(.+)$/); if (!m) continue;
    const r = vers[m[3]] = vers[m[3]] || { logs: 0, crash: 0, os: new Set() };
    r[m[1] === "logs" ? "logs" : "crash"] += k[x][2]; r.os.add(m[2]);
  }
  const list = Object.keys(vers).sort((a, b) => vcmp(b, a));
  const rows = list.length ? list.map((v) => { const r = vers[v];
      // 0.1.0 = a copy built by hand (build.sh / build.cmd), not a release
      return `<tr><td>${esc(v)}${v === "0.1.0" ? ' <span class="sub">(own build)</span>' : ""}</td><td>${[...r.os].join(", ")}</td><td class="n">${r.logs}</td><td class="n">${r.crash ? `<span class="pill bad">${r.crash}</span>` : 0}</td></tr>`; }).join("")
    : `<tr><td colspan="4" class="sub">No logs sent yet (counting started 6 Nov)</td></tr>`;
  return `<h2>Sent logs and crashes</h2>
    <p class="sub" style="margin:-6px 0 10px">Logs sent with "Send log" in the last 30 days, and how many contain an Echo crash ("APP CRASH").</p>
    <div class="wrap"><table><tr><th>Version</th><th>System</th><th class="n">Logs</th><th class="n">With a crash</th></tr>${rows}</table></div>`;
}
function website(s) {
  if (!s) return "";
  const k = s.keys || {}, g = (n) => k[n] || [0, 0, 0];
  const sum = (pre) => Object.keys(k).filter((x) => x.startsWith(pre)).reduce((a, x) => a.map((v, i) => v + k[x][i]), [0, 0, 0]);
  const vis = g("visitors"), views = sum("view:"), dls = sum("dl:");
  // 17 Nov 2026: the developer's own local test pages (127.0.0.1 / localhost) aren't visitors from another site
  const list = (pre, names) => Object.keys(k).filter((x) => x.startsWith(pre) && !/^ref:(127\.|localhost|\[::1\])/.test(x)).sort((a, b) => k[b][2] - k[a][2])
    .map((x) => `<tr><td>${esc(names ? names(x) : x.slice(pre.length))}</td><td class="n">${k[x][0]}</td><td class="n">${k[x][1]}</td><td class="n">${k[x][2]}</td></tr>`).join("")
    || `<tr><td colspan="4" class="sub">Nothing yet</td></tr>`;
  const head = (n) => `<tr><th>${n}</th><th class="n">Today</th><th class="n">7 days</th><th class="n">30 days</th></tr>`;
  const cMax = Math.max(1, ...Object.keys(CLICK_NAMES).map((x) => g(x)[2]));
  const used = Object.keys(CLICK_NAMES).filter((x) => g(x)[2] > 0).sort((a, b) => g(b)[2] - g(a)[2]), unused = Object.keys(CLICK_NAMES).filter((x) => !(g(x)[2] > 0));
  const clicks = used.map((x) => `<tr><td>${CLICK_NAMES[x]}</td><td class="n">${g(x)[0]}</td><td class="n">${g(x)[1]}</td><td class="n">${g(x)[2]}<span class="mini" style="width:${Math.round(g(x)[2] / cMax * 80)}px"></span></td></tr>`).join("")
    + (unused.length ? `<tr class="zero"><td colspan="4">No clicks in 30 days: ${unused.map((x) => esc(CLICK_NAMES[x])).join(" · ")}</td></tr>` : "");
  // 18 Nov 2026: visitors and downloads each get their own chart (on one scale the downloads were slivers)
  const d = s.daily || [], labels = d.map((x) => x.day);
  const visChart = colChart({ title: "Visitors", sub: "unique per day, last 30 days", labels, series: [{ name: "Visitors", c: 1, vals: d.map((x) => x.visitors) }] });
  const hasSplit = d.some((x) => x.dlw != null);
  const dlChart = colChart({ title: "Downloads", sub: "download button clicks per day, last 30 days", labels,
    series: hasSplit ? [{ name: "Windows", c: 1, vals: d.map((x) => x.dlw || 0) }, { name: "Linux", c: 2, vals: d.map((x) => x.dll || 0) }] : [{ name: "Downloads", c: 1, vals: d.map((x) => x.downloads) }] });
  return `
    <h2>Website</h2>
    <div class="grid">
      ${stat("Visitors today", vis[0], `${vis[1]} this week · ${vis[2]} in 30 days`, d.map((x) => x.visitors))}
      ${stat("Page views today", views[0], `${views[1]} this week · ${views[2]} in 30 days`, d.map((x) => x.views))}
      ${stat("Downloads today", dls[0], `${dls[1]} this week · ${dls[2]} in 30 days` + (vis[2] ? ` · about 1 per ${Math.max(1, Math.round(vis[2] / Math.max(1, dls[2])))} visits` : ""), d.map((x) => x.downloads))}
      ${tile("Clicked “See plans”", g("click:plans")[2], `in 30 days · ${g("click:plans")[0]} today`)}
      ${tile("Clicked “Storage”", g("click:storage")[2], `in 30 days · ${g("click:storage")[0]} today`)}
    </div>
    <div class="charts">${visChart}${dlChart}</div>
    <h2>Clicks and downloads</h2>
    <div class="wrap"><table>${head("Button")}${clicks}</table></div>
    <div class="two" style="margin-top:10px">
      <div class="wrap"><table>${head("Page")}${list("view:", (x) => x.slice(5) === "other" ? "other / unknown" : x.slice(5))}</table></div>
      <div class="wrap"><table>${head("Came from")}${list("ref:")}</table></div>
    </div>`;
}

// ----- Echo apps: copies that asked for updates, per UTC day (Flatpak copies update through flatpak - not counted) -----
// 10 Nov 2026: the 2.0.72 test build counts as older than 2.0.0 (like the server's update()), not "newest"
const vnum = (v) => (String(v) === "2.0.72" ? "1.99.0" : String(v)).split(".").map((x) => parseInt(x, 10) || 0);
const vcmp = (a, b) => { const x = vnum(a), y = vnum(b); for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] - y[i]; return 0; };
// share links: views on echoclip.link/s/... (people watching), link previews fetched by Discord & co (= links pasted), most watched
function shareStats(sh, site) {
  if (!sh) return "";
  const k = (site && site.keys) || {}, g = (n) => k[n] || [0, 0, 0];
  const top = (sh.top || []).map((x) => `<tr><td><a href="${SITE}/s/${esc(x.id)}" target="_blank" rel="noopener">${esc(x.id)}</a></td><td>${esc(String(x.key || "").split("/").pop())}</td><td>${esc(x.email || "")}</td><td class="n">${x.views}</td><td>${ago(x.created)}</td></tr>`).join("")
    || `<tr><td colspan="5" class="sub">No views yet</td></tr>`;
  return `<h2>Share links</h2>
    <div class="grid">
      ${tile("Link views today", g("sview")[0], `${g("sview")[1]} this week · ${g("sview")[2]} in 30 days`)}
      ${tile("Discord previews", g("sembed:discord")[2], `link previews fetched in 30 days (a paste can count more than once) · ${g("sembed:discord")[0]} today · ${g("sembed:other")[2]} other apps`)}
      ${tile("Live links", sh.live, `+${sh.new_7d} in 7 days · Echo clips${sh.lasso ? ` · Lasso pictures: ${sh.lasso}` : ""}`)}
    </div>
    <div class="wrap" style="margin-top:10px"><table><tr><th>Link</th><th>Clip</th><th>Owner</th><th class="n">Views</th><th>Made</th></tr>${top}</table></div>`;
}
const flag = (cc) => /^[A-Z]{2}$/.test(cc) ? String.fromCodePoint(...[...cc].map((c) => 0x1f1e6 + c.charCodeAt(0) - 65)) : "";
function appDays(A) {
  const days = [...Array(14)].map((_, i) => new Date(Date.parse(A.today + "T12:00:00Z") - (13 - i) * 864e5).toISOString().slice(0, 10));
  const per = {}; for (const d of days) per[d] = { win: 0, lin: 0, v: {} };
  for (const r of A.rows) {
    const p = per[r.day]; if (!p) continue;
    if (r.k === "app:windows") p.win += r.n; else if (r.k === "app:linux") p.lin += r.n;
    else if (r.k.startsWith("appr:")) p.ret = (p.ret || 0) + r.n;
    else if (r.k.startsWith("inst:")) p.inst = (p.inst || 0) + r.n;
    else if (r.k.startsWith("appv:")) { const [, os, v] = r.k.split(":"); (p.v[v] = p.v[v] || { windows: 0, linux: 0 })[os] += r.n; }
    else if (r.k.startsWith("appch:")) { const [, os, ch] = r.k.split(":"); const c = (p.ch = p.ch || {}); (c[ch] = c[ch] || { windows: 0, linux: 0 })[os] += r.n; }
    else if (r.k.startsWith("appc:")) { const [, , v, cc] = r.k.split(":"); const c = (p.c = p.c || {}); (c[v] = c[v] || {})[cc] = (c[v][cc] || 0) + r.n; }
  }
  return { days, per };
}
// the version table for a set of days (12 Nov 2026: Today / Yesterday / 7 days / 14 days). Over several days a number
// is computer-days: a computer that checked in on 3 of the days counts 3 (the server only keeps per-day counts).
function verTable(per, ds, isNew, label, U) {
  // 18 Nov 2026: different PCs (each computer once, under its newest version in the range) once the server has them
  if (U && U.v) {
    const v = {}, c = {};
    for (const k in U.v) { v[k] = { windows: U.v[k].windows, linux: U.v[k].linux }; c[k] = U.v[k].c || {}; }
    return verRows(v, c, isNew, `${label} · different PCs`) + chanRows(U.ch);
  }
  const v = {}, c = {};
  for (const d of ds) {
    for (const k in per[d].v) { const x = (v[k] = v[k] || { windows: 0, linux: 0 }); x.windows += per[d].v[k].windows; x.linux += per[d].v[k].linux; }
    for (const k in (per[d].c || {})) for (const cc in per[d].c[k]) { const y = (c[k] = c[k] || {}); y[cc] = (y[cc] || 0) + per[d].c[k][cc]; }
  }
  // 21 Nov 2026: Sonela hub apps (Aimdot / Lasso) send their update channel with the daily count (rows appch:<os>:<ch>);
  // copies of older versions don't, they're the "not known" line. Echo itself never has these rows here.
  let ch = null;
  for (const d of ds) for (const k in (per[d].ch || {})) { ch = ch || {}; const x = (ch[k] = ch[k] || { windows: 0, linux: 0 }); x.windows += per[d].ch[k].windows; x.linux += per[d].ch[k].linux; }
  if (ch) {
    const sum = (os) => Object.values(v).reduce((t, x) => t + x[os], 0) - Object.values(ch).reduce((t, x) => t + x[os], 0);
    const rest = { windows: Math.max(0, sum("windows")), linux: Math.max(0, sum("linux")) };
    if (rest.windows + rest.linux) ch[""] = rest;
  }
  return verRows(v, c, isNew, label) + chanRows(ch);
}
// 18 Nov 2026: the same different PCs by update channel (Settings > App Settings > Update channel)
const CHAN = [["stable", "Stable"], ["beta", "Beta"], ["dev", "Dev"], ["", "Not known yet"]];
function chanRows(ch) {
  if (!ch) return "";
  const rows = CHAN.filter(([k]) => ch[k] && ch[k].windows + ch[k].linux > 0).map(([k, l]) => { const x = ch[k];
    return `<tr><td>${l}${k === "" ? ` <span class="sub">(${(curApp() && curApp().id !== "echo") ? "older versions don't say" : "checked in before channels were counted"})</span>` : ""}</td><td class="n">${x.windows}</td><td class="n">${x.linux}</td><td class="sub">${x.windows + x.linux} in all</td></tr>`; }).join("");
  return rows ? `<table style="margin-top:10px"><tr><th>Update channel</th><th class="n">Windows</th><th class="n">Linux</th><th></th></tr>${rows}</table>` : "";
}
function verRows(v, c, isNew, label) {
  const vers = Object.keys(v).filter((k) => v[k].windows + v[k].linux > 0).sort((a, b) => vcmp(b, a));
  const tw = vers.reduce((a, k) => a + v[k].windows, 0), tl = vers.reduce((a, k) => a + v[k].linux, 0);
  const totalRow = vers.length > 1 ? `<tr><td><b>All versions</b></td><td class="n"><b>${tw}</b></td><td class="n"><b>${tl}</b></td><td class="sub">${tw + tl} in all</td><td></td></tr>` : "";
  const rows = vers.length ? vers.map((k) => { const x = v[k], cs = c[k] || {};
      const where = Object.keys(cs).filter((cc) => cs[cc] > 0).sort((a, b) => cs[b] - cs[a]).map((cc) => `${flag(cc)} ${esc(cc)}${cs[cc] > 1 ? " ×" + cs[cc] : ""}`).join(" · ");
      return `<tr><td>${esc(k)}</td><td class="n">${x.windows}</td><td class="n">${x.linux}</td><td class="sub">${where || "-"}</td><td>${isNew(k) === null ? "" : isNew(k) ? '<span class="pill good">newest</span>' : '<span class="pill bad">outdated</span>'}</td></tr>`; }).join("")
    : `<tr><td colspan="5" class="sub">No check-ins in this range</td></tr>`;
  return `<table><tr><th>Version (${label})</th><th class="n">Windows</th><th class="n">Linux</th><th>Where</th><th></th></tr>${rows}${totalRow}</table>`;
}
let VR = "day"; // the version table's range, kept across refreshes
document.addEventListener("click", (e) => { const b = e.target.closest && e.target.closest(".vrb"); if (!b) return;
  VR = b.dataset.r;
  document.querySelectorAll(".vrb").forEach((x) => x.style.borderColor = x === b ? "var(--accent)" : "");
  document.querySelectorAll(".vrt").forEach((x) => x.style.display = x.dataset.r === VR ? "" : "none"); });
function apps(A, site) {
  if (!A) return "";
  const { days, per } = appDays(A);
  const tot = (d) => per[d].win + per[d].lin, today = days[13], yest = days[12];
  // versions: today once it has numbers, else yesterday (the UTC day starts at 1:00 / 2:00 in Denmark)
  // (11 Nov 2026: a version every PC moved away from today sits at 0 - not listed, nor its countries)
  const vd = tot(today) ? today : yest, vers = Object.keys(per[vd].v).filter((v) => per[vd].v[v].windows + per[vd].v[v].linux > 0).sort((a, b) => vcmp(b, a));
  const newest = A.latest, isNew = (v) => (newest ? vcmp(v, newest) >= 0 : null), lite = !!A.lite, nm = appName();
  let onNew = 0, old = 0;
  for (const v of vers) { const n = per[vd].v[v].windows + per[vd].v[v].linux; if (isNew(v)) onNew += n; else old += n; }
  const all7 = days.slice(6, 13).reduce((a, d) => a + tot(d), 0);   // the 7 full days before today
  const pct = (n) => (onNew + old) ? Math.round(n / (onNew + old) * 100) + "%" : "-";
  // 18 Nov 2026: real charts (axis, dates, legend, tooltip) - copies per day, versions in use per day, new installs per day
  const appsChart = colChart({ title: `${nm} apps ${lite ? "in use" : "checked in"}`, sub: lite ? "copies used per day (each counted once a day)" : "different computers per day", labels: days,
    series: [{ name: "Windows", c: 1, vals: days.map((d) => per[d].win) }, { name: "Linux", c: 2, vals: days.map((d) => per[d].lin) }] });
  // versions: the 4 most used in these 14 days, newest first, the rest as "Older"
  const vUse = {}; for (const d of days) for (const v in per[d].v) vUse[v] = (vUse[v] || 0) + per[d].v[v].windows + per[d].v[v].linux;
  const topV = Object.keys(vUse).filter((v) => vUse[v] > 0).sort((a, b) => vUse[b] - vUse[a]).slice(0, 4).sort((a, b) => vcmp(b, a));
  const vn = (d, v) => per[d].v[v] ? per[d].v[v].windows + per[d].v[v].linux : 0;
  const verSeries = topV.map((v, i) => ({ name: v + (v === newest ? " (newest)" : newest && vcmp(v, newest) > 0 ? " (test build)" : ""), c: i + 1, vals: days.map((d) => vn(d, v)) }));
  if (Object.keys(vUse).length > topV.length) verSeries.push({ name: "Older", c: 5, vals: days.map((d) => Object.keys(per[d].v).filter((v) => !topV.includes(v)).reduce((a, v) => a + vn(d, v), 0)) });
  const verChart = colChart({ title: "Versions in use", sub: "computers per day on each version - how fast updates spread", labels: days, series: verSeries });
  const instChart = colChart({ title: "New installs", sub: A.inst_note || "first start of a new copy (Echo 2.0.1+)", labels: days, series: [{ name: "New installs", c: 3, vals: days.map((d) => per[d].inst || 0) }], h: 120 });
  // update speed: share of copies on the newest version, per day (from the version keys)
  const onNewDay = days.map((d) => { let a = 0, t = 0; for (const v in per[d].v) { const n = per[d].v[v].windows + per[d].v[v].linux; t += n; if (isNew(v)) a += n; } return t ? Math.round(a / t * 100) + "%" : "-"; });
  // website downloads next to app copies (rough: one download != one new copy)
  const dlByDay = {}; for (const x of ((site && site.daily) || [])) dlByDay[x.day] = x.downloads;
  const speed = `<div class="wrap" style="margin-top:10px"><table><tr><th>Day</th>${days.slice(7).map((d) => `<th class="n">${d.slice(5)}</th>`).join("")}</tr>
    <tr><td>On newest</td>${onNewDay.slice(7).map((x) => `<td class="n">${x}</td>`).join("")}</tr>
    <tr><td>${lite ? "Apps in use" : "Apps checked in"}</td>${days.slice(7).map((d) => `<td class="n">${tot(d)}</td>`).join("")}</tr>
    <tr><td>Website downloads</td>${days.slice(7).map((d) => `<td class="n">${dlByDay[d] || 0}</td>`).join("")}</tr></table></div>`;
  // what the update check answered (site keys upd:*, [today, 7 days, 30 days])
  const K = (site && site.keys) || {}, U = (n) => K["upd:" + n] || [0, 0, 0];
  const WHY = { stale: "server's GitHub lookup was behind (bug)", "no-signature": "release without signature (broken build)", "no-release": "no release found", paused: "you paused it", hidden: "you hid it", internal: "internal # build", hold: "waiting period" };
  const badNo = Object.keys(WHY).filter((k) => U(k)[2] > 0);
  const updRows = `<tr><td>Offered an update</td><td class="n">${U("offered")[0]}</td><td class="n">${U("offered")[1]}</td><td class="n">${U("offered")[2]}</td></tr>` +
    (badNo.length ? badNo.map((k) => `<tr><td>Told "up to date" but outdated: ${esc(WHY[k])}</td><td class="n">${U(k)[0]}</td><td class="n">${U(k)[1]}</td><td class="n">${U(k)[2]}</td></tr>`).join("")
      : `<tr><td colspan="4" class="sub">Nobody was told "up to date" while a newer version existed</td></tr>`);
  const updTable = `<div class="wrap" style="margin-top:10px"><table><tr><th>Update check answers (PCs per day, added up)</th><th class="n">Today</th><th class="n">7 days</th><th class="n">30 days</th></tr>${updRows}</table></div>`;
  return `
    <h2>${esc(nm)} apps</h2>
    <p class="sub" style="margin:-6px 0 10px">${lite ? esc(A.note || "") : "PCs with Echo: copies that checked for updates, each PC once a day (" + dayStart() + "). Told apart by internet address + system, so roughly one per PC."}</p>
    <div class="grid">
      ${stat("Apps today", tot(today), `${per[today].win} Windows · ${per[today].lin} Linux`, days.map(tot))}
      ${tile("Yesterday", tot(yest), `${per[yest].win} Windows · ${per[yest].lin} Linux`)}
      ${lite ? "" : tile("Returning today", per[today].ret || 0, `also checked in yesterday · ${per[yest].ret || 0} returned yesterday`)}
      ${tile("Per day, last 7 days", (all7 / 7).toFixed(1), "average of the 7 days before today")}
      ${newest ? tile("On the newest version", onNew, `${pct(onNew)} · newest is ${esc(newest)}`) : tile("On the newest version", "-", "no release out yet")}
      ${newest ? tile("Outdated", old, old ? `${pct(old)} haven't updated yet` : "everyone is up to date") : ""}
    </div>
    <div class="charts">${appsChart}${verChart}</div>
    <div class="charts">${instChart}</div>
    <div style="margin-top:10px">${[["day", vd === today ? "Today" : "Yesterday"], ["yest", "Yesterday"], ["7", "7 days"], ["14", "14 days"]].filter((r, i) => i !== 1 || vd === today).map(([r, l]) => `<button class="pstb vrb" data-r="${r}"${r === VR ? ' style="border-color:var(--accent)"' : ""}>${l}</button>`).join(" ")}
      <span class="sub" style="margin-left:8px">${A.uniq && A.uniq.since ? `each computer counted once, under its newest version${A.uniq.since > days[0] ? ` · counting different PCs since ${esc(A.uniq.since)} - older days aren't in it` : ""}` : "over several days: computer-days (a computer counts once per day it checked in)"}</span></div>
    ${[["day", [vd], vd === today ? "today" : "yesterday", vd], ["yest", [yest], "yesterday", yest], ["7", days.slice(7), "7 days", "7"], ["14", days, "14 days", "14"]].map(([r, ds, l, uk]) => `<div class="wrap vrt" data-r="${r}" style="margin-top:10px${r === VR ? "" : ";display:none"}">${verTable(per, ds, isNew, l, A.uniq && A.uniq.since && A.uniq[uk])}</div>`).join("")}
    ${speed}
    ${lite ? "" : updTable}`;
}

// ----- Echo's CPU / memory on real PCs + their specs (10 Nov 2026: perf.rs / specs.rs, sent with the update check) -----
// perf-states (10 Nov 2026): each minute counts under what Echo was doing (perf.rs); "mixed" = reports from older Echo
const PSTATE = {
  all: ["All", "every state together"],
  tray: ["In the tray", "window hidden or minimized"],
  idle: ["Open, not used", "window shown, no mouse / keys in Echo for 60 s+"],
  active: ["In use", "window shown and used: home, library, settings"],
  player: ["Playing a clip", "a clip open in the player or the editor"],
  mixed: ["Mixed (older Echo)", "reports from before the split - all states together"],
};
function perf(P, top) {
  if (!P) return top || "";
  const T = P.total || {}, S = P.by_state || {};
  const pc = (v) => v == null ? "-" : (+v).toFixed(v < 10 ? 2 : 1) + " %";
  const mb = (v) => v == null ? "-" : Math.round(v) + " MB";
  const hrs = (m) => !m ? "0 h" : m < 120 ? m + " min" : (m / 60).toFixed(m < 6000 ? 1 : 0) + " h";
  // 2.0.11+: how much of the CPU is Echo itself (capture + encoder on Windows) and how much its ffmpeg (audio, muxing)
  const split = (own, tot) => own == null || tot == null ? "" : `<div class="sub" title="only PCs on 2.0.11+ report Echo's own share, so this can differ from the total above">2.0.11+: Echo ${pc(own)} · ffmpeg ${pc(Math.max(0, tot - own))}</div>`;
  // 21 Nov 2026 (newer Echo only): where the CPU goes. Inside Echo: its window, the capture + encoder thread (Windows),
  // the audio mixer, and the rest (graphics driver / hardware encoder threads, the clip player). Outside: the recorder's
  // ffmpeg / gpu-screen-recorder and other jobs (clip saves, upload copies, thumbnails)
  const parts = (r) => r.p_all == null || r.p_own == null ? "" : `<div class="sub" title="only Echo versions that report these parts; out of ${pc(r.p_all)} for those PCs">window ${pc(r.p_ui)} · capture ${pc(r.p_cap)} · audio ${pc(r.p_aud)} · driver/player ${pc(Math.max(0, r.p_own - r.p_ui - r.p_cap - r.p_aud))} · recorder ${pc(r.p_rec)} · jobs ${pc(r.p_job)}</div>`;
  const table = (title, rows) => `<div class="wrap" style="margin-top:10px"><table>
    <tr><th>${title}</th><th class="n" title="PC-days: one PC on one day = 1">PC-days</th><th class="n">Recording CPU</th><th class="n">Recording memory</th><th class="n">CPU overall</th><th class="n">Memory overall</th><th class="n">Peak memory</th><th class="n">VRAM</th><th class="n">VRAM peak</th><th class="n">Measured</th></tr>
    ${rows && rows.length ? rows.map((r) => `<tr><td>${esc(r.k || "unknown")}</td><td class="n">${r.pcs}</td><td class="n">${pc(r.rcpu)}${split(r.rcpu_self, r.rcpu_split)}</td><td class="n">${mb(r.rmem)}</td>
      <td class="n">${pc(r.cpu)}${split(r.cpu_self, r.cpu_split)}${parts(r)}</td><td class="n">${mb(r.mem)}</td><td class="n">${mb(r.mem_peak)}</td><td class="n">${mb(r.vram)}</td><td class="n">${mb(r.vram_peak)}</td><td class="n">${hrs(r.mins)}</td></tr>`).join("")
      : `<tr><td colspan="10" class="sub">No reports yet</td></tr>`}</table></div>`;
  // the small text under the recording tiles: how much of the time was recorded, and a warning when it's too little
  // to trust (21 Nov 2026: "how does memory while recording make sense outside of In use?" - recording = the minutes
  // the replay buffer ran, in any state: in the tray = playing with Echo hidden, in use = in Echo while it records)
  const recNote = (t, mem) => !t.rmins ? "nothing recorded in these minutes"
    : `average in the ${hrs(t.rmins)} recorded (${t.mins ? Math.round(t.rmins * 100 / t.mins) : 0} % of the time)${t.rmins < 60 ? " · too little to trust yet" : ""}`;
  // tiles + the six tables for one state (or all of them)
  const block = (st, X) => { const t = X.total || {};
    return `<div class="pst" data-st="${st}"${st === "all" ? "" : ' style="display:none"'}>
    <div class="grid">
      ${st === "all" && P.median_rcpu != null ? tile("Recording CPU, typical PC", pc(P.median_rcpu), `the middle PC-day (half are lower) · overall ${pc(P.median_cpu)}`) : ""}
      ${tile("Recording CPU", t.rmins ? pc(t.rcpu) : "-", recNote(t))}
      ${tile("Recording memory", t.rmins ? mb(t.rmem) : "-", recNote(t, true))}
      ${tile("CPU overall", pc(t.cpu), `recording or not · peak ${t.cpu_peak == null ? "-" : t.cpu_peak + " %"}`)}
      ${tile("Memory overall", mb(t.mem), `recording or not · peak ${mb(t.mem_peak)}`)}
      ${tile("VRAM (graphics memory)", mb(t.vram), `peak ${mb(t.vram_peak)} · Windows, Echo 2.0.7+`)}
      ${tile("Measured", hrs(t.mins), `${t.pcs || 0} PC-days (1 PC on 1 day = 1) · ${esc(PSTATE[st][1])}`)}
    </div>
    <div class="charts">${cpuBars("Recording CPU by graphics card", X.gpu)}${cpuBars("Recording CPU by encoder", X.enc)}</div>
    <h3>Breakdown</h3>
    <div class="seg">${PDIMS.filter(([d]) => d !== "path" || st === "all").map(([d, l]) => `<button class="pdim" data-dim="${d}" aria-pressed="${d === perfDim}">${l}</button>`).join("")}</div>
    ${PDIMS.filter(([d]) => d !== "path" || st === "all").map(([d, l]) => `<div class="pdt" data-dim="${d}"${d === perfDim ? "" : " hidden"}>${table(l, d === "path" ? P.path : X[d])}</div>`).join("")}</div>`; };
  // 18 Nov 2026: the same rows as bars (sorted, PC-days beside each name) - the table below keeps every number
  const cpuBars = (title, rows) => hbars({ title, sub: "average while recording · share of the whole PC, like Task Manager", unit: " %",
    rows: (rows || []).filter((r) => r.rcpu != null && r.rmins > 0).sort((a, b) => b.rcpu - a.rcpu).slice(0, 8)
      .map((r) => ({ k: r.k || "unknown", v: r.rcpu, c: 1, note: r.pcs + (r.pcs === 1 ? " PC-day" : " PC-days") })) });
  const states = (P.states || []).filter((s) => S[s]);
  // one compact row per state: where the minutes went, and what each costs
  const srows = states.map((s) => { const t = S[s].total || {};
    return `<tr><td><b>${esc(PSTATE[s] ? PSTATE[s][0] : s)}</b><div class="sub">${esc(PSTATE[s] ? PSTATE[s][1] : "")}</div></td><td class="n">${t.pcs || 0}</td>
      <td class="n">${hrs(t.mins)}<div class="sub">${T.mins ? Math.round((t.mins || 0) * 100 / T.mins) + " % of all" : ""}</div></td>
      <td class="n">${t.mins ? Math.round((t.rmins || 0) * 100 / t.mins) + " %" : "-"}<div class="sub">${hrs(t.rmins)}</div></td>
      <td class="n">${mb(t.mem)}</td><td class="n">${pc(t.cpu)}</td><td class="n">${t.rmins ? mb(t.rmem) : "-"}</td><td class="n">${t.rmins ? pc(t.rcpu) : "-"}</td><td class="n">${mb(t.mem_peak)}</td>
      <td class="n">${mb(t.vram)}</td><td class="n">${mb(t.vram_peak)}</td></tr>`; }).join("");
  const picker = ["all", ...states].map((s) => `<button class="pstb" data-st="${s}"${s === "all" ? ' style="border-color:var(--accent)"' : ""}>${esc(PSTATE[s] ? PSTATE[s][0] : s)}</button>`).join(" ");
  const recent = (P.recent || []).map((r) => `<tr><td class="sub">${ago(r.at)}</td><td>${esc(PSTATE[r.state] ? PSTATE[r.state][0] : r.state || "Mixed (older Echo)")}</td><td>${esc(r.ver)} · ${esc(r.osname || r.os)}${r.cc ? " · " + flag(r.cc) : ""}</td>
      <td>${esc(r.cpu || "?")}${r.cores ? ` <span class="sub">(${r.cores} threads)</span>` : ""}<div class="sub">${esc(r.gpu || "?")} · ${r.ram ? r.ram + " GB" : "?"}</div></td>
      <td class="sub">${esc(r.res || "?")}${r.fps ? " @ " + r.fps : ""}${r.enc ? "<br>" + esc(r.enc) : ""}${r.encn ? "<br>" + esc(r.encn) : ""}${r.pdec ? "<br>player: " + esc(r.pdec) : ""}${r.aud ? "<br>audio: " + esc(r.aud) : ""}</td>
      <td class="n">${r.rn ? pc(r.rc / 10) : "-"}${r.rn && r.rce != null ? split(r.rce / 10, r.rc / 10) : ""}</td><td class="n">${r.rn ? mb(r.rm) : "-"}</td><td class="n">${pc(r.c / 10)}${r.xr != null && r.ce != null ? parts({ p_all: r.c / 10, p_own: r.ce / 10, p_ui: r.xu / 10, p_cap: r.xc / 10, p_aud: r.xa / 10, p_rec: r.xr / 10, p_job: r.xj / 10 }) : ""}</td><td class="n">${mb(r.m)}</td>
      <td class="n">${mb(r.g)}${r.gp != null ? `<div class="sub">peak ${mb(r.gp)}</div>` : ""}</td><td class="n">${hrs(r.n)}</td></tr>`).join("");
  const recentRows = recent.split("</tr>").filter((x) => x.trim()).map((x) => x + "</tr>");
  return `<details class="sec" open><summary><h2>Performance on people's PCs</h2><span class="sub">last 30 days · Echo ${P.min_ver || "2.0.6"}+ · ${T.pcs || 0} PC-days</span></summary>
    ${top || ""}
    <details class="about"><summary>How these numbers are counted</summary><p class="sub">Echo + its encoder, measured once a minute on each PC (CPU = share of the whole PC, like Task Manager). Sent with the update check, anonymous: numbers and hardware names only. "Recording" = only the minutes Echo's replay buffer was running (a game being recorded), whatever the window was doing; "overall" = every minute, recording or not.
      Each minute counts under what Echo was doing then (the state it was in most of that minute).
      VRAM = Echo's share of the graphics card's own memory (what a game next to it can't use), Windows only, from Echo 2.0.7 - "-" = not measured.
      Only Echo 2.0.11+ (older versions mixed old problems into the averages). "GPU encoder" = the graphics card encodes the video itself (zero-copy), plus minutes when nothing was recorded; "CPU encoder" = the recorder's fallbacks, where the picture goes through the processor (ffmpeg's CPU copy into the card's encoder, or the software x264 encoder) - they cost far more CPU, so they're kept apart. "Capture path" shows both side by side. "Typical PC" = the median PC-day; the averages lean towards the PCs that ran the longest.</p></details>
    <p class="sub" style="margin:8px 0">The window's state and recording are separate: <b>recording</b> = the minutes Echo's replay buffer ran (a game being recorded), whatever the window was doing. "In the tray" + recording = someone playing with Echo hidden - the usual case.</p>
    <div class="wrap"><table><tr><th>What Echo's window was doing</th><th class="n" title="PC-days: one PC on one day = 1">PC-days</th><th class="n">Measured</th><th class="n" title="share of these minutes when the replay buffer ran">Recording</th><th class="n">Memory</th><th class="n">CPU</th><th class="n">Memory while recording</th><th class="n">CPU while recording</th><th class="n">Peak memory</th><th class="n">VRAM</th><th class="n">VRAM peak</th></tr>
      ${srows || `<tr><td colspan="11" class="sub">No reports yet</td></tr>`}</table></div>
    <div id="pstpick" style="margin:12px 0 10px;display:flex;flex-wrap:wrap;gap:6px;align-items:center"><span class="sub">Tiles and tables below for:</span> ${picker}</div>
    ${block("all", P)}${states.map((s) => block(s, S[s])).join("")}
    <h3>Newest reports</h3>
    <div class="wrap"><table><tr><th>When</th><th>State</th><th>Echo · system</th><th>Hardware</th><th>Recording at</th><th class="n">Rec CPU</th><th class="n">Rec memory</th><th class="n">CPU</th><th class="n">Memory</th><th class="n">VRAM</th><th class="n">Measured</th></tr>
      ${recentRows.slice(0, 10).join("") || `<tr><td colspan="11" class="sub">No reports yet</td></tr>`}</table></div>
    ${recentRows.length > 10 ? `<details class="more"><summary>Show ${recentRows.length - 10} older reports</summary><div class="wrap"><table>${recentRows.slice(10).join("")}</table></div></details>` : ""}</details>`;
}
// 18 Nov 2026: Echo 2.0.11+ only (server), split by who encodes the video: the graphics card (zero-copy) or the
// recorder's CPU fallbacks (CPU copy / software x264) - the second costs far more CPU, so the two never mix.
let perfCpu = false, perfSt = "all", perfData = null, perfDim = "gpu";
// the breakdown tables (one shown at a time, 18 Nov 2026 - seven stacked tables made the tab ~6000 px tall)
const PDIMS = [["gpu", "Graphics card"], ["enc", "Encoder"], ["cpu", "Processor"], ["ram", "System memory"], ["res", "Recording at"], ["os", "Windows / Linux"], ["ver", "Echo version"], ["path", "Capture path"]];
function perfBody() {
  const j = perfData || {}, P = perfCpu && j.perf_cpu ? j.perf_cpu : j.perf;
  const b = (v, label) => `<button class="pverb" data-cpu="${v ? 1 : 0}"${perfCpu === v ? ' style="border-color:var(--accent)"' : ""}>${label}</button>`;
  return perf(P, `<div id="perfver" style="margin:10px 0 0;display:flex;flex-wrap:wrap;gap:6px;align-items:center">${b(false, "GPU encoder")} ${b(true, "CPU encoder")}
    <span class="sub">GPU = the graphics card encodes (zero-copy) · CPU = a fallback where the picture goes through the processor</span></div>`);
}
function perfSec(j) { perfData = j; return `<div id="perfbox">${perfBody()}</div>`; }
// the state buttons above the tiles: show that state's tiles + tables; the version buttons redraw the section
function perfPick() {
  const vb = $("#perfver");
  if (vb) vb.onclick = (e) => { const b = e.target.closest(".pverb"); if (!b || !$("#perfbox")) return;
    perfCpu = b.dataset.cpu === "1"; $("#perfbox").innerHTML = perfBody(); perfPick();
    const s = document.querySelector(`.pstb[data-st="${perfSt}"]`); if (s) s.click(); };
  const pb = $("#perfbox");
  if (pb) pb.onclick = (e) => { const b = e.target.closest(".pdim"); if (!b) return; perfDim = b.dataset.dim;
    document.querySelectorAll(".pdim").forEach((x) => x.setAttribute("aria-pressed", String(x.dataset.dim === perfDim)));
    document.querySelectorAll(".pdt").forEach((x) => x.hidden = x.dataset.dim !== perfDim); };
  const box = $("#pstpick"); if (!box) return;
  box.onclick = (e) => { const b = e.target.closest(".pstb"); if (!b) return; perfSt = b.dataset.st;
    document.querySelectorAll(".pstb").forEach((x) => x.style.borderColor = x === b ? "var(--accent)" : "");
    document.querySelectorAll(".pst").forEach((d) => d.style.display = d.dataset.st === b.dataset.st ? "" : "none"); };
}

// ----- releases: fix the "What changed?" text, hide a release, pause it as an update (chat 28) -----
function releases(list) {
  if (!list || !list.length) return "";
  const rows = list.map((r) => { const hid = !String(r.notes || "").trim() || String(r.notes).trim().startsWith("#");
    return `<tr data-v="${esc(r.version)}"><td><b>${esc(r.version)}</b><div class="sub">${day(r.at)}</div></td>
      <td style="width:55%"><textarea class="rnotes" rows="2" style="width:100%">${esc(r.notes || "")}</textarea></td>
      <td style="white-space:nowrap">${r.out === false ? '<span class="pill">building - or the build failed</span>' : ""}${hid ? '<span class="pill bad">hidden</span>' : ""}${r.paused ? '<span class="pill bad">updates paused</span>' : ""}</td>
      <td style="white-space:nowrap"><button class="rsave">Save</button> <button class="rhide">${hid ? "Hidden" : "Hide"}</button>
        <button class="rpause">${r.paused ? "Resume updates" : "Pause updates"}</button><div class="sub rmsg"></div></td></tr>`; }).join("");
  const building = list.some((r) => r.out === false);
  return `<details class="sec"${building ? " open" : ""}><summary><h2>Releases</h2><span class="sub">${list.length} versions${building ? " · one is building" : ""}</span></summary>
    <p class="sub" style="margin:10px 0 10px">The text shows on echoclip.link/changelog, in Echo's "What's new" and in the update offer. Empty or starting with # = hidden.
      New releases are offered as an update right away; fixing the text later still updates it everywhere. "Pause updates" stops a version from being offered (downloads on the website still work) - a new version shows up here within a minute of publishing, so you can pause it while GitHub is still building it.</p>
    <div class="wrap"><table id="rels"><tr><th>Version</th><th>What changed</th><th></th><th></th></tr>${rows}</table></div></details>`;
}

// ----- tabs (11 Nov 2026): the chosen tab is in the address (#apps, ...) so a link opens it, and remembered in this browser -----
const TABS = [["overview", "Overview"], ["apps", "Apps & versions"], ["perf", "Performance & PCs"], ["web", "Website"], ["cloud", "Echo Cloud"], ["mod", "Moderation"]];
let VIS = TABS; // the tabs the shown app has numbers for (draw sets it)
const isTab = (t) => VIS.some((x) => x[0] === t);
function curTab() {
  const h = location.hash.slice(1); if (isTab(h)) return h;
  let t = ""; try { t = localStorage.getItem("echo_admin_tab") || ""; } catch { }
  return isTab(t) ? t : "overview";
}
function showTab(t) {
  const bar = document.querySelector(".tabs"); if (!bar) return;
  document.querySelectorAll(".tab").forEach((x) => x.hidden = x.dataset.tab !== t);
  bar.querySelectorAll("a").forEach((a) => a.classList.toggle("on", a.dataset.tab === t));
  const on = bar.querySelector("a.on"); if (on) bar.scrollLeft = Math.max(0, on.offsetLeft - (bar.clientWidth - on.offsetWidth) / 2); // phone: keep it in view
  try { localStorage.setItem("echo_admin_tab", t); } catch { }
  if (location.hash.slice(1) !== t) history.replaceState(null, "", "#" + t);
}
window.addEventListener("hashchange", () => {
  showTab(curTab());
  // a new tab starts at its top (measured from the title row: the sticky bar's own offsetTop moves with it)
  const top = document.querySelector(".top"), y = top ? top.offsetTop + top.offsetHeight : 0;
  if (top && window.scrollY > y) window.scrollTo(0, y);
});

function draw(j) {
  // lite = an app counted by the Sonela hub (sonela.app/admin): daily downloads, installs, users and versions - no
  // Echo Cloud, money, performance, website or moderation parts (their tabs are hidden)
  const lite = !!j.lite;
  const has = { overview: true, apps: !!j.apps, perf: !!j.perf, web: !!j.site && !lite, cloud: !!j.users, mod: !!j.users };
  VIS = TABS.filter(([t]) => has[t]);
  const m = j.money || { currency: "EUR", r2_usd_month: 0, domain_usd_month: 0, net_month: 0, gross_month: 0, paying: 0 }, cur = m.currency;
  // costs are in USD (Cloudflare bills USD); shown next to EUR income, close enough for a glance
  const costs = m.r2_usd_month + m.domain_usd_month;
  const left = m.net_month - costs;
  const verdict = left >= 0 ? `<span class="pill good">▲ pays for itself</span>` : `<span class="pill bad">▼ costs you money</span>`;
  const u = j.users || {}, a = j.active || {};
  // Echo installs, all time (10 Nov 2026): Echo 2.0.1+ sends a one-time "new install" with its first update check
  const I = j.installs || { total: 0, windows: 0, linux: 0, d1: 0, d7: 0 };
  // subscriptions that pay nothing (100% codes) or from before amounts were tracked aren't income
  const extra = (m.discounted_free ? ` · ${m.discounted_free} free via code` : "") + (m.unknown ? ` · ${m.unknown} older, not counted` : "");
  const rows = (list, f) => list.length ? list.map(f).join("") : `<tr><td colspan="9" class="sub">None</td></tr>`;
  // tabs (11 Nov 2026): everything is drawn once from the one /admin/stats answer; a tab only shows / hides its part
  const sec = (t, html) => `<section class="tab" data-tab="${t}" hidden>${html}</section>`;
  const A = j.apps ? appDays(j.apps) : null, at = A && A.per[A.days[13]], ay = A && A.per[A.days[12]];
  const SK = (j.site && j.site.keys) || {}, sk = (n) => SK[n] || [0, 0, 0];
  const dls = Object.keys(SK).filter((x) => x.startsWith("dl:")).reduce((t, x) => t.map((v, i) => v + SK[x][i]), [0, 0, 0]);
  $("#app").innerHTML = `
    <div class="top">${switcher()}<span class="sub" id="upd" data-at="${j.at}" title="${esc(when(j.at))}">updated just now</span><span class="sp"></span>
      <select id="tz" title="Time zone for the times and dates on this page" aria-label="Time zone">${TZS.map(([z, n]) => `<option value="${z}"${z === TZ ? " selected" : ""}>${n}</option>`).join("")}</select>
      <span class="sub keys" title="keyboard: 1-${VIS.length} switch tabs, R refreshes">keys 1–${VIS.length} · R</span><button id="re">Refresh</button><button id="out">Sign out</button></div>
    <nav class="tabs">${VIS.map(([t, n]) => `<a href="#${t}" data-tab="${t}">${n}${t === "mod" ? '<span class="cnt" id="modcnt" hidden></span>' : ""}</a>`).join("")}</nav>

    ${sec("overview", `
    ${attention(j, A)}
    ${funnel(j, I, sk("visitors"), dls)}
    <h2>Today <span class="sub" style="font-weight:400;font-size:13px">${dayStart()} · arrows compare full days</span></h2>
    <div class="grid">
      ${A ? stat(`PCs with ${esc(appName())} today`, at.win + at.lin, `${at.win} Windows · ${at.lin} Linux · ${ay.win + ay.lin} yesterday`, A.days.map((d) => A.per[d].win + A.per[d].lin)) : ""}
      ${A && !lite ? stat("Returning today", at.ret || 0, `also checked in yesterday · ${ay.ret || 0} returned yesterday`, A.days.map((d) => A.per[d].ret || 0)) : ""}
      ${stat("New installs today", I.d1, `+${I.d7} in 7 days · ${I.total} since 10 Nov`, A ? A.days.map((d) => A.per[d].inst || 0) : null)}
      ${j.site ? stat("Downloads today", dls[0], `${dls[1]} this week · ${dls[2]} in 30 days`, (j.site.daily || []).map((x) => x.downloads)) : ""}
      ${j.site && !lite ? stat("Visitors today", sk("visitors")[0], `${sk("visitors")[1]} this week · ${sk("visitors")[2]} in 30 days`, (j.site.daily || []).map((x) => x.visitors)) : ""}
    </div>

    ${lite ? `<h2>People</h2><div class="grid">${tile(`${esc(appName())} installs (all time)`, I.total, I.total ? `+${I.d1} today · +${I.d7} this week · ${I.windows} Windows · ${I.linux} Linux` : "no installs counted yet")}</div>` : `
    <h2>Money per month</h2>
    <div class="grid">
      ${tile("Left over" + verdict, money(left, cur), "income after fees + VAT, minus costs (rough, on the low side)", "hero")}
      ${tile("Income", money(m.gross_month, cur), `${m.paying} paying · ${money(m.net_month, cur)} after Polar + VAT${extra}`)}
      ${tile("Storage cost (R2)", "$" + m.r2_usd_month.toFixed(2), `${gb(j.storage.used_bytes)} stored · first 10 GB free`)}
      ${tile("Domain", "$" + m.domain_usd_month.toFixed(2), "echoclip.link, $7.20 a year")}
    </div>

    <h2>People</h2>
    <div class="grid">
      ${tile("Echo installs since 10 Nov 2026", I.total, I.total ? `+${I.d1} today · +${I.d7} in 7 days · ${I.windows} Windows · ${I.linux} Linux · counted from Echo 2.0.1 on (the first days include people updating from older versions)` : "counting starts with Echo 2.0.1")}
      ${tile("Echo Cloud accounts", u.total, `+${u.new_1d} in 24 h · +${u.new_7d} in 7 days · +${u.new_30d} in 30 days`)}
      ${tile("Echo Cloud users, last 24 h", a.d1, `${a.d7} in 7 days · ${a.d30} in 30 days · your admin page not counted`)}
      ${tile("Paying", u.paid, u.total ? `${(u.paid / u.total * 100).toFixed(1)}% of accounts` + (u.paid > m.paying ? ` · ${u.paid - m.paying} with an unknown amount (older plans)` : "") : "")}
      ${tile("Over their plan", u.over, u.over ? "clips get removed after 30 days" : "nobody")}
      ${tile("Echo share links", j.shares.live, `live now · +${j.shares.new_7d} in 7 days · ${j.shares.total} saved (removed ones not counted)${j.shares.lasso ? ` · Lasso: ${j.shares.lasso} more` : ""}`)}
      ${tile("Profiles", j.profiles ? j.profiles.total : 0, j.profiles ? `+${j.profiles.new_7d} this week · ${j.profiles.follows} follows` : "")}
      ${tile("Logs on file (kept 14 days)", j.logs, j.backup ? "last backup " + esc(j.backup.replace("backups/db-", "").replace(".json.gz", "")) : "no backup yet")}
    </div>`}`)}

    ${sec("apps", `${apps(j.apps, j.site)}
    ${lite ? "" : crashes(SK)}
    ${releases(j.releases)}`)}

    ${has.perf ? sec("perf", perfSec(j)) : ""}

    ${has.web ? sec("web", website(j.site)) : ""}

    ${has.cloud ? sec("cloud", `
    <h2>Plans</h2>
    <div class="wrap"><table><tr><th>Plan</th><th class="n">Accounts</th><th class="n">Stored</th></tr>
      ${rows(j.plans, (p) => `<tr><td>${esc(p.plan)}</td><td class="n">${p.n}</td><td class="n">${gb(p.used)}</td></tr>`)}</table></div>

    <h2>Biggest accounts</h2>
    <div class="wrap"><table><tr><th>Email</th><th>Plan</th><th class="n">Stored</th><th>Of plan</th><th>Last counted</th><th>Joined</th></tr>
      ${rows(j.top, (t) => { const p = t.quota ? t.used / t.quota : 0; return `<tr><td>${esc(t.email)}</td><td>${esc(t.plan)}</td><td class="n">${gb(t.used)}</td>
        <td>${Math.round(p * 100)}%<span class="bar"><i class="${p > 1 ? "full" : ""}" style="width:${Math.min(100, p * 100).toFixed(0)}%"></i></span></td><td>${ago(t.used_at)}</td><td>${day(t.created)}</td></tr>`; })}</table></div>

    <h2>Over their plan</h2>
    <div class="wrap"><table><tr><th>Email</th><th class="n">Stored</th><th class="n">Plan</th><th>Warned</th><th>Clips removed on</th></tr>
      ${rows(j.over, (o) => `<tr><td>${esc(o.email)}</td><td class="n">${gb(o.used)}</td><td class="n">${gb(o.quota)}</td><td>${day(o.over_since)}${o.over_stage >= 2 ? " + reminder" : ""}</td><td>${day(o.over_since + 30 * 86400)}</td></tr>`)}</table></div>

    <h2>Newest accounts</h2>
    <div class="wrap"><table><tr><th>Email</th><th>Plan</th><th>Joined</th><th></th></tr>
      ${rows(j.newest, (n) => `<tr><td>${esc(n.email)}</td><td>${esc(n.plan)}</td><td>${ago(n.created)}</td><td><button class="mkmod" data-e="${esc(n.email)}">Make moderator</button></td></tr>`)}</table></div>

    ${shareStats(j.shares, j.site)}`) : ""}

    ${has.mod ? sec("mod", `
    <div id="modsec"><h2>Moderators and reports</h2><div class="sub">Loading…</div></div>

    <h2>Profiles</h2>
    <div class="tile" style="margin-bottom:12px"><div class="k">Change an @name</div>
      <div style="display:flex;gap:8px;flex-wrap:wrap;margin:8px 0">
        <input id="hwho" placeholder="their @name, /u/ link or email" style="flex:1;min-width:200px">
        <input id="hto" placeholder="new @name (empty = random user_123456)" style="flex:1;min-width:200px"></div>
      <label class="sub"><input type="checkbox" id="hblock"> block the old @name so nobody can take it again (inappropriate names)</label>
      <div class="sub" style="margin-top:6px">Reserved names (echo, support…) are allowed here. Empty = a random @name, and their display name is reset too; it stays until they pick a new one in Echo.</div>
      <div style="margin-top:10px"><button id="hgo">Change @name</button> <span class="sub" id="hmsg"></span></div></div>
    <div class="wrap"><table><tr><th>@name</th><th>Name</th><th>Email</th><th class="n">Followers</th><th class="n">Pinned</th><th>Made</th><th></th></tr>
      ${rows((j.profiles && j.profiles.list) || [], (q) => `<tr><td><a href="${SITE}/u/${esc(q.handle)}" target="_blank" rel="noopener">@${esc(q.handle)}</a></td><td>${esc(q.name || "")}</td><td>${esc(q.email || "")}</td><td class="n">${q.followers}</td><td class="n">${q.pins}</td><td>${ago(q.created)}</td><td><button class="hpick" data-h="${esc(q.handle)}">Change</button></td></tr>`)}</table></div>

    <h2>Remove something</h2>
    <div class="two">
      <div class="tile"><div class="k">Reported share link</div>
        <input id="rmshare" placeholder="https://echoclip.link/s/…" style="width:100%;margin:8px 0">
        <label class="sub"><input type="checkbox" id="rmclip"> also delete the clip itself</label>
        <div style="margin-top:10px"><button id="rmsharego">Remove link</button> <span class="sub" id="rmsharemsg"></span></div></div>
      <div class="tile"><div class="k">Reported profile</div>
        <input id="rmprof" placeholder="@name or https://echoclip.link/u/…" style="width:100%;margin:8px 0">
        <label class="sub"><input type="checkbox" id="rmblock" checked> block the @name so nobody can take it again</label>
        <div style="margin-top:10px"><button id="rmprofgo">Remove profile</button> <span class="sub" id="rmprofmsg"></span></div></div>
    </div>`) : ""}

    ${lite ? `<details class="about note"><summary>How these numbers are counted</summary>${esc(j.note || "")}</details>` : `<details class="about note"><summary>How these numbers are counted</summary>Only Echo Cloud accounts are counted (people who only record or use their own cloud aren't visible here).
      Storage is the last count per account. Income is what each subscription pays per month after discounts (yearly plans divided by 12), before Polar's fee and VAT;
      Polar → Finance has the exact numbers. Costs: Cloudflare R2 storage + the domain; Workers and D1 are on the free plan.
      Website: counted without cookies or stored IP addresses (bots and scripts skipped); "visitors" are unique per day, days are UTC.
      "Came from" only lists other websites (Discord, Reddit, Google…); direct visits aren't listed.</details>`}`;
  showTab(curTab());
  $("#re").onclick = () => load(true);
  $("#tz").onchange = (e) => { TZ = e.target.value; try { localStorage.setItem("sonela_admin_tz", TZ); } catch { } draw(j); };
  perfPick();
  if (perfSt !== "all") { const b = document.querySelector(`.pstb[data-st="${perfSt}"]`); if (b) b.click(); }
  tickUpdated();
  // takedowns: the owner's email comes back so you can write to them if needed
  const post = async (path, b, out) => {
    out.textContent = "…";
    try {
      const r = await fetch(API + path, { method: "POST", headers: auth({ "content-type": "application/json" }), body: JSON.stringify(b) });
      const x = await r.json().catch(() => ({}));
      out.textContent = r.ok ? `Done${x.owner ? " · owner " + x.owner : ""}${x.blocked ? " · @name blocked" : ""}` : (x.error || "Error " + r.status);
    } catch { out.textContent = "Couldn't reach echoclip.link"; }
  };
  // releases
  const rels = $("#rels");
  if (rels) rels.addEventListener("click", async (e) => {
    const btn = e.target.closest("button"); if (!btn) return;
    const tr = btn.closest("tr"), v = tr.dataset.v, out = tr.querySelector(".rmsg"), ta = tr.querySelector(".rnotes");
    let b = null;
    if (btn.classList.contains("rsave")) b = { version: v, notes: ta.value };
    else if (btn.classList.contains("rhide")) { if (!confirm(`Hide ${v} from the changelog and updates?`)) return; b = { version: v, notes: "" }; }
    else if (btn.classList.contains("rpause")) b = { version: v, paused: btn.textContent.startsWith("Pause") };
    if (!b) return;
    out.textContent = "…";
    try {
      const r = await fetch(API + "/admin/release", { method: "POST", headers: auth({ "content-type": "application/json" }), body: JSON.stringify(b) });
      const x = await r.json().catch(() => ({}));
      if (r.ok) load(); else out.textContent = x.error || "Error " + r.status;
    } catch { out.textContent = "Couldn't reach echoclip.link"; }
  });
  if (has.mod) {
  $("#rmsharego").onclick = () => { const v = $("#rmshare").value.trim(); if (v && confirm("Remove this share link?")) post("/admin/takedown", { id: v, delete_clip: $("#rmclip").checked }, $("#rmsharemsg")); };
  $("#rmprofgo").onclick = () => { const v = $("#rmprof").value.trim(); if (v && confirm("Remove this profile?")) post("/admin/profile-takedown", { handle: v, block: $("#rmblock").checked }, $("#rmprofmsg")); };
  document.querySelectorAll(".hpick").forEach((b) => b.onclick = () => { $("#hwho").value = "@" + b.dataset.h; $("#hto").focus(); $("#hwho").scrollIntoView({ block: "center" }); });
  $("#hgo").onclick = async () => {
    const who = $("#hwho").value.trim(), to = $("#hto").value.trim(), out = $("#hmsg");
    if (!who || !confirm(to ? `Change ${who} to @${to.replace(/^@/, "")}?` : `Give ${who} a random @name?`)) return;
    out.textContent = "…";
    try {
      const r = await fetch(API + "/admin/profile-handle", { method: "POST", headers: auth({ "content-type": "application/json" }), body: JSON.stringify({ who, to, block_old: $("#hblock").checked }) });
      const x = await r.json().catch(() => ({}));
      out.textContent = r.ok ? `Done: ${x.from ? "@" + x.from + " → " : ""}@${x.to}${x.blocked ? " · old @name blocked" : ""}${x.owner ? " · " + x.owner : ""}` : (x.error || "Error " + r.status);
    } catch { out.textContent = "Couldn't reach echoclip.link"; }
  };
  document.querySelectorAll(".mkmod").forEach((b) => b.onclick = () => modsPost({ email: b.dataset.e }, `Make ${b.dataset.e} a moderator?`));
  loadMods();
  }
  $("#out").onclick = () => { fetch(API + "/auth/logout", { method: "POST", headers: auth() }).catch(() => { }); try { localStorage.removeItem("echo_admin_token"); } catch { } key = ""; login(); };
}
// ----- moderators (chat 29): they sign in at echoclip.link/mod with their Echo Cloud email -----
async function loadMods() {
  let r; try { r = await fetch(API + "/admin/mods", { headers: auth() }); } catch { return; }
  if (r.ok) drawMods(await r.json());
}
async function modsPost(b, ask) {
  if (ask && !confirm(ask)) return;
  const out = $("#modmsg"); if (out) out.textContent = "…";
  try {
    const r = await fetch(API + "/admin/mods", { method: "POST", headers: auth({ "content-type": "application/json" }), body: JSON.stringify(b) });
    const x = await r.json().catch(() => ({}));
    if (r.ok) drawMods(x); else if (out) out.textContent = x.error || "Error " + r.status; else alert(x.error || "Error " + r.status);
  } catch { if (out) out.textContent = "Couldn't reach echoclip.link"; }
}
function drawMods(m) {
  const box = $("#modsec"); if (!box) return;
  const mods = m.mods.length ? m.mods.map((x) => `<tr><td>${esc(x.email)}</td><td>${ago(x.added)}</td><td>${x.seen ? ago(x.seen) : "-"}</td><td><button class="rmmod" data-e="${esc(x.email)}">Remove</button></td></tr>`).join("")
    : `<tr><td colspan="4" class="sub">No moderators yet</td></tr>`;
  const reps = m.open.length ? m.open.map((o) => `<tr><td><a href="${SITE}${o.kind === "s" ? "/s/" : "/u/"}${esc(o.target)}" target="_blank" rel="noopener">${o.kind === "s" ? "/s/" : "@"}${esc(o.target)}</a>${o.exists ? "" : ' <span class="sub">(gone)</span>'}</td>
      <td class="n">${o.n}</td><td>${o.items.map((i) => esc(i.reason) + (i.details ? ": " + esc(i.details) : "") + (i.contact ? ` <span class="sub">(${esc(i.contact)})</span>` : "")).join("<br>")}</td><td>${ago(o.last)}</td></tr>`).join("")
    : `<tr><td colspan="4" class="sub">No open reports</td></tr>`;
  const log = m.log.length ? m.log.slice(0, 15).map((l) => `<tr><td>${ago(l.at)}</td><td>${esc(l.who)}</td><td>${esc(l.action)}</td><td>${esc(l.target)}</td></tr>`).join("") : `<tr><td colspan="4" class="sub">Nothing yet</td></tr>`;
  const cnt = $("#modcnt"); if (cnt) { cnt.textContent = m.open.length; cnt.hidden = !m.open.length; }
  // the Overview's "Needs attention" list learns about open reports here (they load after the page)
  const am = $("#attmod"); if (am && m.open.length) { am.hidden = false; am.querySelector(".att-t").textContent = `${m.open.length} open report${m.open.length === 1 ? "" : "s"} to look at`; const ok = $("#attok"); if (ok) ok.hidden = true; }
  box.innerHTML = `<h2>Moderators and reports</h2>
    <p class="sub" style="margin:-6px 0 10px">Moderators sign in at <a href="${SITE}/mod" target="_blank" rel="noopener">echoclip.link/mod</a> with their Echo Cloud email. They can handle reports, remove share links and profiles and change @names - not delete clips or accounts, and they never see emails.</p>
    <div class="two"><div><div class="wrap"><table><tr><th>Moderator</th><th>Added</th><th>Last active</th><th></th></tr>${mods}</table></div>
      <div style="display:flex;gap:8px;margin-top:10px"><input id="modem" placeholder="email of their Echo Cloud account" style="flex:1"><button id="modadd">Add</button></div><div class="sub" id="modmsg"></div></div>
    <div><div class="wrap"><table><tr><th>When</th><th>Who</th><th>What</th><th>On</th></tr>${log}</table></div></div></div>
    <h2>Open reports (${m.open.length})</h2>
    <div class="wrap"><table><tr><th>What</th><th class="n">Reports</th><th>Why (reporter's email if given)</th><th>Last</th></tr>${reps}</table></div>`;
  $("#modadd").onclick = () => { const e = $("#modem").value.trim(); if (e) modsPost({ email: e }); };
  box.querySelectorAll(".rmmod").forEach((b) => b.onclick = () => modsPost({ email: b.dataset.e, remove: true }, `Remove ${b.dataset.e} as moderator?`));
}
load();
