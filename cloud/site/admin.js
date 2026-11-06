// echoclip.link/admin (chat 28: moved out of admin.html - the page's Content-Security-Policy only runs script files)
const $ = (s) => document.querySelector(s);
const esc = (t) => String(t ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const gb = (b) => { b = b || 0; return b >= 1e12 ? (b / 1e12).toFixed(2) + " TB" : b >= 1e9 ? (b / 1e9).toFixed(b >= 100e9 ? 0 : 1) + " GB" : (b / 1e6).toFixed(0) + " MB"; };
const ago = (t) => { if (!t) return "-"; const d = Date.now() / 1000 - t; return d < 3600 ? Math.round(d / 60) + " min ago" : d < 86400 ? Math.round(d / 3600) + " h ago" : Math.round(d / 86400) + " days ago"; };
const day = (t) => t ? new Date(t * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" }) : "-";
const money = (v, c) => (v || 0).toLocaleString(undefined, { style: "currency", currency: c || "EUR" });
let key = ""; try { key = sessionStorage.getItem("echo_admin_key") || ""; } catch { }

function login(msg) {
  $("#app").innerHTML = `<div class="login"><h1>Echo stats</h1><div class="sub">Paste your ADMIN_KEY (Bitwarden). It's remembered in this browser only.</div>
    <input id="k" type="password" placeholder="ADMIN_KEY" autocomplete="off"><button id="go">Open</button>${msg ? `<div class="err">${esc(msg)}</div>` : ""}</div>`;
  $("#go").onclick = () => { key = $("#k").value.trim(); try { sessionStorage.setItem("echo_admin_key", key); } catch { } load(); };
  $("#k").onkeydown = (e) => { if (e.key === "Enter") $("#go").click(); };
  $("#k").focus();
}

async function load() {
  if (!key) return login();
  let r;
  try { r = await fetch("/admin/stats", { headers: { "x-admin-key": key } }); } catch { return login("Couldn't reach echoclip.link"); }
  if (r.status === 403) { try { sessionStorage.removeItem("echo_admin_key"); } catch { } return login("Wrong key"); }
  if (!r.ok) return login("Error " + r.status);
  draw(await r.json());
}

function tile(k, v, s, cls) { return `<div class="tile ${cls || ""}"><div class="k">${k}</div><div class="v">${v}</div>${s ? `<div class="s">${s}</div>` : ""}</div>`; }

// ----- website (counted without cookies; days are UTC) -----
const CLICK_NAMES = { "click:plans": "See plans / Pricing", "click:storage": "Storage (menu)", "click:features": "Features (menu)",
  "click:download": "Download (jump to buttons)", "click:discord": "Discord", "click:changelog": "Changelog", "click:github": "GitHub",
  "dl:windows": "Download: Windows installer", "dl:windows-portable": "Download: Echo.exe (no install)", "dl:linux": "Download: Linux AppImage", "dl:linux-tar": "Download: Linux tar.gz", "dl:flatpak": "Download: Linux Flatpak" };
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
      return `<tr><td>${esc(v)}</td><td>${[...r.os].join(", ")}</td><td class="n">${r.logs}</td><td class="n">${r.crash ? `<span class="pill bad">${r.crash}</span>` : 0}</td></tr>`; }).join("")
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
  const list = (pre, names) => Object.keys(k).filter((x) => x.startsWith(pre)).sort((a, b) => k[b][2] - k[a][2])
    .map((x) => `<tr><td>${esc(names ? names(x) : x.slice(pre.length))}</td><td class="n">${k[x][0]}</td><td class="n">${k[x][1]}</td><td class="n">${k[x][2]}</td></tr>`).join("")
    || `<tr><td colspan="4" class="sub">Nothing yet</td></tr>`;
  const head = (n) => `<tr><th>${n}</th><th class="n">Today</th><th class="n">7 days</th><th class="n">30 days</th></tr>`;
  const clicks = Object.keys(CLICK_NAMES).map((x) => `<tr><td>${CLICK_NAMES[x]}</td><td class="n">${g(x)[0]}</td><td class="n">${g(x)[1]}</td><td class="n">${g(x)[2]}</td></tr>`).join("");
  // 30 days: visitors (bars) with downloads drawn inside them
  const d = s.daily || [], max = Math.max(1, ...d.map((x) => Math.max(x.visitors, x.downloads))), W = 600, H = 150, bw = W / Math.max(1, d.length);
  const bars = d.map((x, i) => { const hv = x.visitors / max * (H - 4), hd = x.downloads / max * (H - 4);
    return `<g><title>${x.day}: ${x.visitors} visitors, ${x.views} page views, ${x.downloads} downloads</title><rect x="${i * bw}" y="0" width="${bw}" height="${H}" fill="transparent"/>
      <rect x="${i * bw + 2}" y="${H - hv}" width="${bw - 4}" height="${hv}" rx="2" fill="var(--accent)" opacity=".35"/>
      <rect x="${i * bw + 2}" y="${H - hd}" width="${bw - 4}" height="${hd}" rx="2" fill="var(--accent)"/></g>`; }).join("");
  const short = (x) => x ? new Date(x + "T12:00:00Z").toLocaleDateString(undefined, { day: "numeric", month: "short" }) : "";
  return `
    <h2>Website</h2>
    <div class="grid">
      ${tile("Visitors today", vis[0], `${vis[1]} this week · ${vis[2]} in 30 days`)}
      ${tile("Page views today", views[0], `${views[1]} this week · ${views[2]} in 30 days`)}
      ${tile("Downloads today", dls[0], `${dls[1]} this week · ${dls[2]} in 30 days` + (vis[2] ? ` · ${Math.round(dls[2] / vis[2] * 100)}% of visitors` : ""))}
      ${tile("Clicked “See plans”", g("click:plans")[2], `in 30 days · ${g("click:plans")[0]} today`)}
      ${tile("Clicked “Storage”", g("click:storage")[2], `in 30 days · ${g("click:storage")[0]} today`)}
    </div>
    <div class="chart"><div class="lg"><span><i style="background:var(--accent);opacity:.35"></i>Visitors</span><span><i style="background:var(--accent)"></i>Downloads</span><span style="margin-left:auto">last 30 days · hover a day</span></div>
      <svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">${bars}</svg><div class="ax"><span>${short(d[0] && d[0].day)}</span><span>max ${max} a day</span><span>today</span></div></div>
    ${crashes(k)}
    <h2>Clicks and downloads</h2>
    <div class="wrap"><table>${head("Button")}${clicks}</table></div>
    <div class="two" style="margin-top:10px">
      <div class="wrap"><table>${head("Page")}${list("view:", (x) => x.slice(5) === "other" ? "other / unknown" : x.slice(5))}</table></div>
      <div class="wrap"><table>${head("Came from")}${list("ref:")}</table></div>
    </div>`;
}

// ----- Echo apps: copies that asked for updates, per UTC day (Flatpak copies update through flatpak - not counted) -----
const vnum = (v) => String(v).split(".").map((x) => parseInt(x, 10) || 0);
const vcmp = (a, b) => { const x = vnum(a), y = vnum(b); for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] - y[i]; return 0; };
// share links: views on echoclip.link/s/... (people watching), link previews fetched by Discord & co (= links pasted), most watched
function shareStats(sh, site) {
  if (!sh) return "";
  const k = (site && site.keys) || {}, g = (n) => k[n] || [0, 0, 0];
  const top = (sh.top || []).map((x) => `<tr><td><a href="/s/${esc(x.id)}" target="_blank">${esc(x.id)}</a></td><td>${esc(String(x.key || "").split("/").pop())}</td><td>${esc(x.email || "")}</td><td class="n">${x.views}</td><td>${ago(x.created)}</td></tr>`).join("")
    || `<tr><td colspan="5" class="sub">No views yet</td></tr>`;
  return `<h2>Share links</h2>
    <div class="grid">
      ${tile("Link views today", g("sview")[0], `${g("sview")[1]} this week · ${g("sview")[2]} in 30 days`)}
      ${tile("Pasted in Discord", g("sembed:discord")[2], `previews in 30 days · ${g("sembed:discord")[0]} today · ${g("sembed:other")[2]} other apps`)}
      ${tile("Live links", sh.live, `+${sh.new_7d} this week`)}
    </div>
    <div class="wrap" style="margin-top:10px"><table><tr><th>Link</th><th>Clip</th><th>Owner</th><th class="n">Views</th><th>Made</th></tr>${top}</table></div>`;
}
const flag = (cc) => /^[A-Z]{2}$/.test(cc) ? String.fromCodePoint(...[...cc].map((c) => 0x1f1e6 + c.charCodeAt(0) - 65)) : "";
function apps(A, site) {
  if (!A) return "";
  const days = [...Array(14)].map((_, i) => new Date(Date.parse(A.today + "T12:00:00Z") - (13 - i) * 864e5).toISOString().slice(0, 10));
  const per = {}; for (const d of days) per[d] = { win: 0, lin: 0, v: {} };
  for (const r of A.rows) {
    const p = per[r.day]; if (!p) continue;
    if (r.k === "app:windows") p.win += r.n; else if (r.k === "app:linux") p.lin += r.n;
    else if (r.k.startsWith("appv:")) { const [, os, v] = r.k.split(":"); (p.v[v] = p.v[v] || { windows: 0, linux: 0 })[os] += r.n; }
    else if (r.k.startsWith("appc:")) { const [, , v, cc] = r.k.split(":"); const c = (p.c = p.c || {}); (c[v] = c[v] || {})[cc] = (c[v][cc] || 0) + r.n; }
  }
  const tot = (d) => per[d].win + per[d].lin, today = days[13], yest = days[12];
  // versions: today once it has numbers, else yesterday (the UTC day starts at 1:00 in Denmark)
  const vd = tot(today) ? today : yest, vers = Object.keys(per[vd].v).sort((a, b) => vcmp(b, a));
  const newest = A.latest, isNew = (v) => newest && vcmp(v, newest) >= 0;
  let onNew = 0, old = 0;
  for (const v of vers) { const n = per[vd].v[v].windows + per[vd].v[v].linux; if (isNew(v)) onNew += n; else old += n; }
  const all7 = days.slice(7).reduce((a, d) => a + tot(d), 0);
  const pct = (n) => (onNew + old) ? Math.round(n / (onNew + old) * 100) + "%" : "-";
  const vrows = vers.length ? vers.map((v) => { const x = per[vd].v[v];
      const cs = (per[vd].c && per[vd].c[v]) || {}, where = Object.keys(cs).sort((a, b) => cs[b] - cs[a]).map((cc) => `${flag(cc)} ${esc(cc)}${cs[cc] > 1 ? " ×" + cs[cc] : ""}`).join(" · ");
      return `<tr><td>${esc(v)}</td><td class="n">${x.windows}</td><td class="n">${x.linux}</td><td class="sub">${where || "-"}</td><td>${isNew(v) ? '<span class="pill good">newest</span>' : '<span class="pill bad">outdated</span>'}</td></tr>`; }).join("")
    : `<tr><td colspan="5" class="sub">No check-ins yet - they start after this server update</td></tr>`;
  const max = Math.max(1, ...days.map(tot)), W = 600, H = 120, bw = W / 14;
  const bars = days.map((d, i) => { const hw = per[d].win / max * (H - 4), hl = per[d].lin / max * (H - 4);
    return `<g><title>${d}: ${tot(d)} copies (${per[d].win} Windows, ${per[d].lin} Linux)</title><rect x="${i * bw}" y="0" width="${bw}" height="${H}" fill="transparent"/>
      <rect x="${i * bw + 3}" y="${H - hw - hl}" width="${bw - 6}" height="${hl}" rx="2" fill="var(--accent)" opacity=".45"/>
      <rect x="${i * bw + 3}" y="${H - hw}" width="${bw - 6}" height="${hw}" rx="2" fill="var(--accent)"/></g>`; }).join("");
  // update speed: share of copies on the newest version, per day (from the version keys)
  const onNewDay = days.map((d) => { let a = 0, t = 0; for (const v in per[d].v) { const n = per[d].v[v].windows + per[d].v[v].linux; t += n; if (isNew(v)) a += n; } return t ? Math.round(a / t * 100) + "%" : "-"; });
  // website downloads next to app copies (rough: one download != one new copy)
  const dlByDay = {}; for (const x of ((site && site.daily) || [])) dlByDay[x.day] = x.downloads;
  const speed = `<div class="wrap" style="margin-top:10px"><table><tr><th>Day</th>${days.slice(7).map((d) => `<th class="n">${d.slice(5)}</th>`).join("")}</tr>
    <tr><td>On newest</td>${onNewDay.slice(7).map((x) => `<td class="n">${x}</td>`).join("")}</tr>
    <tr><td>Apps checked in</td>${days.slice(7).map((d) => `<td class="n">${tot(d)}</td>`).join("")}</tr>
    <tr><td>Website downloads</td>${days.slice(7).map((d) => `<td class="n">${dlByDay[d] || 0}</td>`).join("")}</tr></table></div>`;
  // what the update check answered (site keys upd:*, [today, 7 days, 30 days])
  const K = (site && site.keys) || {}, U = (n) => K["upd:" + n] || [0, 0, 0];
  const WHY = { stale: "server's GitHub lookup was behind (bug)", "no-signature": "release without signature (broken build)", "no-release": "no release found", paused: "you paused it", hidden: "you hid it", internal: "internal # build", hold: "waiting period" };
  const badNo = Object.keys(WHY).filter((k) => U(k)[2] > 0);
  const updRows = `<tr><td>Offered an update</td><td class="n">${U("offered")[0]}</td><td class="n">${U("offered")[1]}</td><td class="n">${U("offered")[2]}</td></tr>` +
    (badNo.length ? badNo.map((k) => `<tr><td>Told "up to date" but outdated: ${esc(WHY[k])}</td><td class="n">${U(k)[0]}</td><td class="n">${U(k)[1]}</td><td class="n">${U(k)[2]}</td></tr>`).join("")
      : `<tr><td colspan="4" class="sub">Nobody was told "up to date" while a newer version existed</td></tr>`);
  const updTable = `<div class="wrap" style="margin-top:10px"><table><tr><th>Update check answers (computers)</th><th class="n">Today</th><th class="n">7 days</th><th class="n">30 days</th></tr>${updRows}</table></div>`;
  return `
    <h2>Echo apps</h2>
    <p class="sub" style="margin:-6px 0 10px">Copies of Echo that checked for updates, once per computer per day (UTC). Flatpak copies update through Flatpak and aren't counted.</p>
    <div class="grid">
      ${tile("Apps today", tot(today), `${per[today].win} Windows · ${per[today].lin} Linux`)}
      ${tile("Yesterday", tot(yest), `${per[yest].win} Windows · ${per[yest].lin} Linux`)}
      ${tile("Per day, last 7 days", (all7 / 7).toFixed(1), "average")}
      ${tile("On the newest version", onNew, `${pct(onNew)} · newest is ${esc(newest || "?")}`)}
      ${tile("Outdated", old, old ? `${pct(old)} haven't updated yet` : "everyone is up to date")}
    </div>
    <div class="chart"><div class="lg"><span><i style="background:var(--accent)"></i>Windows</span><span><i style="background:var(--accent);opacity:.45"></i>Linux</span><span style="margin-left:auto">last 14 days · hover a day</span></div>
      <svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">${bars}</svg><div class="ax"><span>${days[0].slice(5)}</span><span>max ${max} a day</span><span>today</span></div></div>
    <div class="wrap" style="margin-top:10px"><table><tr><th>Version (${vd === today ? "today" : "yesterday"})</th><th class="n">Windows</th><th class="n">Linux</th><th>Where</th><th></th></tr>${vrows}</table></div>
    ${speed}
    ${updTable}`;
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

function draw(j) {
  const m = j.money, cur = m.currency;
  // costs are in USD (Cloudflare bills USD); shown next to EUR income, close enough for a glance
  const costs = m.r2_usd_month + m.domain_usd_month;
  const left = m.net_month - costs;
  const verdict = left >= 0 ? `<span class="pill good">▲ pays for itself</span>` : `<span class="pill bad">▼ costs you money</span>`;
  const u = j.users, a = j.active;
  // subscriptions that pay nothing (100% codes) or from before amounts were tracked aren't income
  const extra = (m.discounted_free ? ` · ${m.discounted_free} free via code` : "") + (m.unknown ? ` · ${m.unknown} older, not counted` : "");
  const rows = (list, f) => list.length ? list.map(f).join("") : `<tr><td colspan="9" class="sub">None</td></tr>`;
  $("#app").innerHTML = `
    <div class="top"><h1>Echo stats</h1><span class="sub">updated ${new Date(j.at * 1000).toLocaleTimeString()}</span><span class="sp"></span>
      <button id="re">Refresh</button><button id="out">Forget key</button></div>

    <h2>Money per month</h2>
    <div class="grid">
      ${tile("Left over" + verdict, money(left, cur), "income after fees + VAT, minus costs", "hero")}
      ${tile("Income", money(m.gross_month, cur), `${m.paying} paying · ${money(m.net_month, cur)} after Polar + VAT${extra}`)}
      ${tile("Storage cost (R2)", "$" + m.r2_usd_month.toFixed(2), `${gb(j.storage.used_bytes)} stored · first 10 GB free`)}
      ${tile("Domain", "$" + m.domain_usd_month.toFixed(2), "echoclip.link, $7.20 a year")}
    </div>

    <h2>People</h2>
    <div class="grid">
      ${tile("Echo Cloud accounts", u.total, `+${u.new_1d} today · +${u.new_7d} this week · +${u.new_30d} in 30 days`)}
      ${tile("Echo Cloud users today", a.d1, `${a.d7} this week · ${a.d30} in 30 days`)}
      ${tile("Paying", m.paying, u.total ? `${(m.paying / u.total * 100).toFixed(1)}% of accounts` + (u.paid > m.paying ? ` · ${u.paid} with a plan` : "") : "")}
      ${tile("Over their plan", u.over, u.over ? "clips get removed after 30 days" : "nobody")}
      ${tile("Share links", j.shares.live, `+${j.shares.new_7d} this week · ${j.shares.total} made`)}
      ${tile("Profiles", j.profiles ? j.profiles.total : 0, j.profiles ? `+${j.profiles.new_7d} this week · ${j.profiles.follows} follows` : "")}
      ${tile("Logs sent (14 days)", j.logs, j.backup ? "last backup " + esc(j.backup.replace("backups/db-", "").replace(".json.gz", "")) : "no backup yet")}
    </div>

    ${apps(j.apps, j.site)}
    ${shareStats(j.shares, j.site)}

    ${website(j.site)}

    <h2>Plans</h2>
    <div class="wrap"><table><tr><th>Plan</th><th class="n">Accounts</th><th class="n">Stored</th></tr>
      ${rows(j.plans, (p) => `<tr><td>${esc(p.plan)}</td><td class="n">${p.n}</td><td class="n">${gb(p.used)}</td></tr>`)}</table></div>

    <h2>Biggest accounts</h2>
    <div class="wrap"><table><tr><th>Email</th><th>Plan</th><th class="n">Stored</th><th>Of plan</th><th>Counted</th><th>Joined</th></tr>
      ${rows(j.top, (t) => { const p = t.quota ? t.used / t.quota : 0; return `<tr><td>${esc(t.email)}</td><td>${esc(t.plan)}</td><td class="n">${gb(t.used)}</td>
        <td>${Math.round(p * 100)}%<span class="bar"><i class="${p > 1 ? "full" : ""}" style="width:${Math.min(100, p * 100).toFixed(0)}%"></i></span></td><td>${ago(t.used_at)}</td><td>${day(t.created)}</td></tr>`; })}</table></div>

    <h2>Over their plan</h2>
    <div class="wrap"><table><tr><th>Email</th><th class="n">Stored</th><th class="n">Plan</th><th>Warned</th><th>Clips removed on</th></tr>
      ${rows(j.over, (o) => `<tr><td>${esc(o.email)}</td><td class="n">${gb(o.used)}</td><td class="n">${gb(o.quota)}</td><td>${day(o.over_since)}${o.over_stage >= 2 ? " + reminder" : ""}</td><td>${day(o.over_since + 30 * 86400)}</td></tr>`)}</table></div>

    <h2>Newest accounts</h2>
    <div class="wrap"><table><tr><th>Email</th><th>Plan</th><th>Joined</th><th></th></tr>
      ${rows(j.newest, (n) => `<tr><td>${esc(n.email)}</td><td>${esc(n.plan)}</td><td>${ago(n.created)}</td><td><button class="mkmod" data-e="${esc(n.email)}">Make moderator</button></td></tr>`)}</table></div>

    <h2>Profiles</h2>
    <div class="tile" style="margin-bottom:12px"><div class="k">Change an @name</div>
      <div style="display:flex;gap:8px;flex-wrap:wrap;margin:8px 0">
        <input id="hwho" placeholder="their @name, /u/ link or email" style="flex:1;min-width:200px">
        <input id="hto" placeholder="new @name (empty = random user_123456)" style="flex:1;min-width:200px"></div>
      <label class="sub"><input type="checkbox" id="hblock"> block the old @name so nobody can take it again (inappropriate names)</label>
      <div class="sub" style="margin-top:6px">Reserved names (echo, support…) are allowed here. Empty = a random @name, and their display name is reset too; it stays until they pick a new one in Echo.</div>
      <div style="margin-top:10px"><button id="hgo">Change @name</button> <span class="sub" id="hmsg"></span></div></div>
    <div class="wrap"><table><tr><th>@name</th><th>Name</th><th>Email</th><th class="n">Followers</th><th class="n">Pinned</th><th>Made</th><th></th></tr>
      ${rows((j.profiles && j.profiles.list) || [], (q) => `<tr><td><a href="/u/${esc(q.handle)}" target="_blank" rel="noopener">@${esc(q.handle)}</a></td><td>${esc(q.name || "")}</td><td>${esc(q.email || "")}</td><td class="n">${q.followers}</td><td class="n">${q.pins}</td><td>${ago(q.created)}</td><td><button class="hpick" data-h="${esc(q.handle)}">Change</button></td></tr>`)}</table></div>

    ${releases(j.releases)}

    <div id="modsec"><h2>Moderators and reports</h2><div class="sub">Loading…</div></div>

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
    </div>

    <div class="note">Only Echo Cloud accounts are counted (people who only record or use their own cloud aren't visible here).
      Storage is the last count per account. Income is what each subscription pays per month after discounts (yearly plans divided by 12), before Polar's fee and VAT;
      Polar → Finance has the exact numbers. Costs: Cloudflare R2 storage + the domain; Workers and D1 are on the free plan.
      Website: counted without cookies or stored IP addresses (bots and scripts skipped); "visitors" are unique per day, days are UTC.
      "Came from" only lists other websites (Discord, Reddit, Google…); direct visits aren't listed.</div>`;
  $("#re").onclick = load;
  // takedowns: the owner's email comes back so you can write to them if needed
  const post = async (path, b, out) => {
    out.textContent = "…";
    try {
      const r = await fetch(path, { method: "POST", headers: { "x-admin-key": key, "content-type": "application/json" }, body: JSON.stringify(b) });
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
      const r = await fetch("/admin/release", { method: "POST", headers: { "x-admin-key": key, "content-type": "application/json" }, body: JSON.stringify(b) });
      const x = await r.json().catch(() => ({}));
      if (r.ok) load(); else out.textContent = x.error || "Error " + r.status;
    } catch { out.textContent = "Couldn't reach echoclip.link"; }
  });
  $("#rmsharego").onclick = () => { const v = $("#rmshare").value.trim(); if (v && confirm("Remove this share link?")) post("/admin/takedown", { id: v, delete_clip: $("#rmclip").checked }, $("#rmsharemsg")); };
  $("#rmprofgo").onclick = () => { const v = $("#rmprof").value.trim(); if (v && confirm("Remove this profile?")) post("/admin/profile-takedown", { handle: v, block: $("#rmblock").checked }, $("#rmprofmsg")); };
  document.querySelectorAll(".hpick").forEach((b) => b.onclick = () => { $("#hwho").value = "@" + b.dataset.h; $("#hto").focus(); $("#hwho").scrollIntoView({ block: "center" }); });
  $("#hgo").onclick = async () => {
    const who = $("#hwho").value.trim(), to = $("#hto").value.trim(), out = $("#hmsg");
    if (!who || !confirm(to ? `Change ${who} to @${to.replace(/^@/, "")}?` : `Give ${who} a random @name?`)) return;
    out.textContent = "…";
    try {
      const r = await fetch("/admin/profile-handle", { method: "POST", headers: { "x-admin-key": key, "content-type": "application/json" }, body: JSON.stringify({ who, to, block_old: $("#hblock").checked }) });
      const x = await r.json().catch(() => ({}));
      out.textContent = r.ok ? `Done: ${x.from ? "@" + x.from + " → " : ""}@${x.to}${x.blocked ? " · old @name blocked" : ""}${x.owner ? " · " + x.owner : ""}` : (x.error || "Error " + r.status);
    } catch { out.textContent = "Couldn't reach echoclip.link"; }
  };
  document.querySelectorAll(".mkmod").forEach((b) => b.onclick = () => modsPost({ email: b.dataset.e }, `Make ${b.dataset.e} a moderator?`));
  loadMods();
  $("#out").onclick = () => { try { sessionStorage.removeItem("echo_admin_key"); } catch { } key = ""; login(); };
}
// ----- moderators (chat 29): they sign in at echoclip.link/mod with their Echo Cloud email -----
async function loadMods() {
  let r; try { r = await fetch("/admin/mods", { headers: { "x-admin-key": key } }); } catch { return; }
  if (r.ok) drawMods(await r.json());
}
async function modsPost(b, ask) {
  if (ask && !confirm(ask)) return;
  const out = $("#modmsg"); if (out) out.textContent = "…";
  try {
    const r = await fetch("/admin/mods", { method: "POST", headers: { "x-admin-key": key, "content-type": "application/json" }, body: JSON.stringify(b) });
    const x = await r.json().catch(() => ({}));
    if (r.ok) drawMods(x); else if (out) out.textContent = x.error || "Error " + r.status; else alert(x.error || "Error " + r.status);
  } catch { if (out) out.textContent = "Couldn't reach echoclip.link"; }
}
function drawMods(m) {
  const box = $("#modsec"); if (!box) return;
  const mods = m.mods.length ? m.mods.map((x) => `<tr><td>${esc(x.email)}</td><td>${ago(x.added)}</td><td>${x.seen ? ago(x.seen) : "-"}</td><td><button class="rmmod" data-e="${esc(x.email)}">Remove</button></td></tr>`).join("")
    : `<tr><td colspan="4" class="sub">No moderators yet</td></tr>`;
  const reps = m.open.length ? m.open.map((o) => `<tr><td><a href="${o.kind === "s" ? "/s/" : "/u/"}${esc(o.target)}" target="_blank" rel="noopener">${o.kind === "s" ? "/s/" : "@"}${esc(o.target)}</a>${o.exists ? "" : ' <span class="sub">(gone)</span>'}</td>
      <td class="n">${o.n}</td><td>${o.items.map((i) => esc(i.reason) + (i.details ? ": " + esc(i.details) : "") + (i.contact ? ` <span class="sub">(${esc(i.contact)})</span>` : "")).join("<br>")}</td><td>${ago(o.last)}</td></tr>`).join("")
    : `<tr><td colspan="4" class="sub">No open reports</td></tr>`;
  const log = m.log.length ? m.log.slice(0, 15).map((l) => `<tr><td>${ago(l.at)}</td><td>${esc(l.who)}</td><td>${esc(l.action)}</td><td>${esc(l.target)}</td></tr>`).join("") : `<tr><td colspan="4" class="sub">Nothing yet</td></tr>`;
  box.innerHTML = `<h2>Moderators and reports</h2>
    <p class="sub" style="margin:-6px 0 10px">Moderators sign in at <a href="/mod" target="_blank">echoclip.link/mod</a> with their Echo Cloud email. They can handle reports, remove share links and profiles and change @names - not delete clips or accounts, and they never see emails.</p>
    <div class="two"><div><div class="wrap"><table><tr><th>Moderator</th><th>Added</th><th>Last seen</th><th></th></tr>${mods}</table></div>
      <div style="display:flex;gap:8px;margin-top:10px"><input id="modem" placeholder="email of their Echo Cloud account" style="flex:1"><button id="modadd">Add</button></div><div class="sub" id="modmsg"></div></div>
    <div><div class="wrap"><table><tr><th>When</th><th>Who</th><th>What</th><th>On</th></tr>${log}</table></div></div></div>
    <h2>Open reports (${m.open.length})</h2>
    <div class="wrap"><table><tr><th>What</th><th class="n">Reports</th><th>Why (reporter's email if given)</th><th>Last</th></tr>${reps}</table></div>`;
  $("#modadd").onclick = () => { const e = $("#modem").value.trim(); if (e) modsPost({ email: e }); };
  box.querySelectorAll(".rmmod").forEach((b) => b.onclick = () => modsPost({ email: b.dataset.e, remove: true }, `Remove ${b.dataset.e} as moderator?`));
}
load();
