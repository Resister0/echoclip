// echoclip.link: download buttons for the visitor's system, changelog page, theme picker + drifting particles in the hero
// anonymous counts for Echo's stats page: page views and a few button clicks (no cookies, nothing stored in the browser)
(() => {
  if (!/^https?:$/.test(location.protocol) || /^(localhost|127\.)/.test(location.hostname)) return;
  const send = (o) => { try {
    const b = JSON.stringify(o);
    if (!(navigator.sendBeacon && navigator.sendBeacon("/hit", new Blob([b], { type: "text/plain" })))) fetch("/hit", { method: "POST", body: b, keepalive: true }).catch(() => {});
  } catch { /* never mind */ } };
  send({ t: "view", p: location.pathname, r: document.referrer });
  document.addEventListener("click", (e) => {
    const a = e.target.closest && e.target.closest("a[href]");
    if (!a) return;
    const h = a.getAttribute("href");
    const k = /discord\.(gg|com)/.test(h) ? "discord" : /github\.com/.test(h) ? "github" : /^\/pricing/.test(h) ? "plans"
      : /#storage$/.test(h) ? "storage" : /#features$/.test(h) ? "features" : /^\/changelog/.test(h) ? "changelog" : /#download$|^\/download\/?$/.test(h) ? "download" : "";
    if (k) send({ t: "click", k });
  }, true);
})();

(() => {
  // Linux visitors see the Linux download first (and highlighted)
  const ua = navigator.userAgent || "", linux = /Linux|X11/.test(ua) && !/Android/.test(ua);
  const dl = document.querySelector(".dl"), win = dl && dl.querySelector('[data-os="windows"]'), lin = dl && dl.querySelector('[data-os="linux"]');
  if (linux && win && lin) {
    dl.insertBefore(lin, win); lin.classList.add("primary"); win.classList.remove("primary");
    // the new homepage (11 Nov 2026) uses the kit's button classes too: swap them along
    if (win.classList.contains("btn--primary")) { win.classList.replace("btn--primary", "btn--secondary"); lin.classList.replace("btn--secondary", "btn--primary"); const arrow = win.querySelector(".m-arrow"); if (arrow) lin.append(arrow); }
  }
  // /download "All downloads": on Linux the Linux cards come first and the AppImage (= the big Linux button) is the
  // purple, recommended one (chat 28)
  const grid = document.querySelector(".dlgrid");
  if (linux && grid) {
    const cards = [...grid.querySelectorAll(".card[data-os]")];
    cards.filter((c) => c.dataset.os === "linux").reverse().forEach((c) => grid.prepend(c));
    const main = grid.querySelector('.card[data-os="linux"][data-main]'), rec = grid.querySelector(".rec");
    // (the new pages also carry the kit classes btn--primary / btn--secondary: swapped along)
    grid.querySelectorAll(".btn.primary").forEach((b) => { b.classList.remove("primary"); b.classList.replace("btn--primary", "btn--secondary"); });
    if (main) { const mb = main.querySelector(".btn"); mb.classList.add("primary"); mb.classList.replace("btn--secondary", "btn--primary"); if (rec) main.querySelector("p").append(" ", rec); }
    const sub = document.getElementById("dlsub");
    if (sub) sub.textContent = "The AppImage is the easy choice. The Flatpak brings everything it needs - best for Steam Deck and Bazzite.";
  }
  // /download: the latest version under the buttons
  const dv = document.getElementById("dlver");
  if (dv) fetch("/changes").then((r) => r.json()).then((j) => {
    const x = (j.releases || [])[0]; if (!x) return;
    const d = new Date(x.at * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
    dv.innerHTML = `Echo ${String(x.version).replace(/[^0-9.]/g, "")} · ${d} · Windows 10/11 and Linux · <a href="/changelog">What's new</a>`;
  }).catch(() => {});
  // /changelog (chat 28): newest version as a big card, the rest grouped by day; each "What changed?" text is split
  // into its parts (commas) and every part gets a tag (New / Fix / Linux / Windows) that the filter buttons use
  const box = document.getElementById("changes");
  if (box) fetch("/changes").then((r) => r.json()).then((j) => {
    const esc = (t) => String(t).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
    const day = (t) => new Date(t * 1000).toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long", year: "numeric" });
    const short = (t) => new Date(t * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
    const list = j.releases || [];
    if (!list.length) { box.innerHTML = '<p class="sub">No versions listed yet.</p>'; return; }
    // split on commas + sentence ends that aren't inside quotes or brackets; first letter up. A comma right after
    // "Something: Word" is a list ("Light themes: Light, Paper and Match system") and doesn't split (chat 31)
    const parts = (n) => { const out = []; let cur = "", q = false, br = 0; const t = String(n);
      for (let i = 0; i < t.length; i++) { const ch = t[i];
        if (ch === '"') q = !q; else if (ch === "(") br++; else if (ch === ")" && br) br--;
        const top = !q && !br;
        if (top && ch === "," && !/:\s*\S+$/.test(cur)) { out.push(cur); cur = ""; }
        else if (top && ch === "." && /^\s+[A-Z0-9]/.test(t.slice(i + 1, i + 3))) { out.push(cur); cur = ""; }
        else cur += ch; }
      out.push(cur); return out.map((x) => x.trim().replace(/\.$/, "")).filter(Boolean).map((x) => x[0].toUpperCase() + x.slice(1)); };
    const tags = (t) => { const g = [];
      if (/\b(fix|fixed|fixes|no longer|crash|bug|broken|stuck|works again|instead of)\b/i.test(t)) g.push("fix"); else g.push("new");
      if (/linux|appimage|flatpak|kde|wayland|nobara|steam deck|bazzite/i.test(t)) g.push("linux");
      if (/windows|installer|echo\.exe|smartscreen/i.test(t)) g.push("windows");
      return g; };
    const NAME = { new: "New", fix: "Fix", linux: "Linux", windows: "Windows" };
    const items = (n) => `<ul>${parts(n).map((p) => { const g = tags(p);
      return `<li data-t="${g.join(" ")}">${g.map((x) => `<span class="tg ${x}">${NAME[x]}</span>`).join("")}<span>${esc(p)}</span></li>`; }).join("")}</ul>`;
    const [first, ...rest] = list;
    let html = `<div class="cfilter" role="group" aria-label="Show">${["all", "new", "fix", "linux", "windows"].map((f, i) =>
      `<button type="button" data-f="${f}"${i ? "" : ' class="on"'}>${f === "all" ? "Everything" : f === "fix" ? "Fixes" : NAME[f]}</button>`).join("")}</div>
      <article class="clatest"><div class="chead"><b>Echo ${esc(first.version)}</b><span class="new">Latest</span><time>${short(first.at)}</time></div>${items(first.notes)}</article>`;
    let lastDay = "";
    for (const x of rest) {
      const d = day(x.at);
      if (d !== lastDay) { html += `<h3 class="cday">${esc(d)}</h3>`; lastDay = d; }
      html += `<article class="crel"><div class="chead"><b>${esc(x.version)}</b></div>${items(x.notes)}</article>`;
    }
    html += `<p class="sub cnote">Missing version numbers were internal test builds that never went out.</p>`;
    box.innerHTML = html;
    box.querySelector(".cfilter").addEventListener("click", (e) => {
      const b = e.target.closest("button"); if (!b) return;
      box.querySelectorAll(".cfilter button").forEach((x) => x.classList.toggle("on", x === b));
      const f = b.dataset.f;
      box.querySelectorAll("li[data-t]").forEach((li) => { li.hidden = f !== "all" && !li.dataset.t.split(" ").includes(f); });
      box.querySelectorAll("article").forEach((a) => { a.hidden = ![...a.querySelectorAll("li")].some((li) => !li.hidden); });
      box.querySelectorAll(".cday").forEach((h) => { let n = h.nextElementSibling, any = false;
        while (n && n.tagName === "ARTICLE") { if (!n.hidden) any = true; n = n.nextElementSibling; } h.hidden = !any; });
    });
  }).catch(() => { box.innerHTML = '<p class="sub">Couldn\'t load the changelog - try again in a moment.</p>'; });
})();

(() => {
  const THEMES = [
    ["", "Violet", "#7c5cff"], ["ocean", "Ocean", "#2fa8e0"], ["ember", "Ember", "#f07a1a"], ["emerald", "Emerald", "#2fbf86"],
    ["crimson", "Crimson", "#d0505a"], ["rose", "Rosé", "#e0609e"], ["slate", "Slate", "#8a96a8"],
  ];
  const root = document.documentElement, btn = document.getElementById("themebtn"), box = document.getElementById("themes");
  const current = () => root.dataset.theme || "";
  const set = (t) => {
    if (t) root.dataset.theme = t; else delete root.dataset.theme;
    try { t ? localStorage.setItem("echo-theme", t) : localStorage.removeItem("echo-theme"); } catch (e) { /* private mode */ }
    box.querySelectorAll("button").forEach((b) => b.classList.toggle("on", b.dataset.t === t));
  };
  if (btn && box) {
    for (const [t, name, col] of THEMES) {
      const b = document.createElement("button");
      b.type = "button"; b.dataset.t = t;
      b.innerHTML = `<i style="background:linear-gradient(135deg,${col},${col}88)"></i>${name}`;
      b.onclick = () => set(t);
      box.appendChild(b);
    }
    set(current());
    btn.onclick = (e) => { e.stopPropagation(); box.hidden = !box.hidden; btn.setAttribute("aria-expanded", String(!box.hidden)); };
    document.addEventListener("click", (e) => { if (!box.hidden && !box.contains(e.target)) { box.hidden = true; btn.setAttribute("aria-expanded", "false"); } });
    document.addEventListener("keydown", (e) => { if (e.key === "Escape") box.hidden = true; });
  }

  // particles: small dots drifting up in the accent colour, like bcso.xyz's backgrounds
  const c = document.getElementById("sky");
  if (!c || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const ctx = c.getContext("2d");
  let w, h, dots = [];
  const size = () => {
    const r = c.getBoundingClientRect(), d = Math.min(devicePixelRatio || 1, 2);
    w = r.width; h = r.height; c.width = w * d; c.height = h * d; ctx.setTransform(d, 0, 0, d, 0, 0);
    dots = Array.from({ length: Math.round(w * h / 9000) }, () => ({ x: Math.random() * w, y: Math.random() * h, r: Math.random() * 1.6 + .4, s: Math.random() * .25 + .05, a: Math.random() * .6 + .2, p: Math.random() * 6.3 }));
  };
  size(); addEventListener("resize", size);
  let run = true;
  document.addEventListener("visibilitychange", () => { run = !document.hidden; if (run) requestAnimationFrame(tick); });
  function tick(t) {
    if (!run) return;
    const col = getComputedStyle(root).getPropertyValue("--accent-rgb").trim() || "124,92,255";
    ctx.clearRect(0, 0, w, h);
    for (const d of dots) {
      d.y -= d.s; d.x += Math.sin(t / 2000 + d.p) * .15;
      if (d.y < -4) { d.y = h + 4; d.x = Math.random() * w; }
      ctx.globalAlpha = d.a * (.6 + .4 * Math.sin(t / 900 + d.p));
      ctx.fillStyle = `rgb(${col})`;
      ctx.beginPath(); ctx.arc(d.x, d.y, d.r, 0, 6.3); ctx.fill();
    }
    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
})();

// homepage app preview: hover shows a clip's buttons like in Echo, Copy ▾ opens the real menu (share link, Discord sizes);
// pressing anything explains what it does in Echo and points to the download - nothing actually happens
(() => {
  const app = document.querySelector(".app"); if (!app) return;
  const svg = (d) => `<svg viewBox="0 0 24 24">${d}</svg>`;
  const ICON = {
    edit: svg('<path d="M4 20h4L19 9l-4-4L4 16z"/>'),
    link: svg('<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>'),
    folder: svg('<path d="M3 7h6l2 2h10v10H3z"/>'),
    del: svg('<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/>'),
  };
  const SAY = {
    copy: "copies the clip - paste it straight into Discord or anywhere",
    full: "copies the full-quality clip",
    free: "shrinks the clip to fit Discord's free upload limit, then copies it",
    basic: "shrinks the clip to fit Nitro Basic, then copies it",
    nitro: "copies the clip sized for Nitro",
    link: "uploads the clip and copies a link that plays right inside Discord",
    edit: "opens the editor: trim, captions, zoom, stickers and more",
    folder: "shows the clip in its folder",
    del: "moves the clip to the Recycle Bin",
    save: "saves the last minutes you played as a clip",
    rec: "starts recording by hand, like a normal recorder",
  };
  const box = document.createElement("div"); box.className = "atoasts"; (app.querySelector(".amain") || app).appendChild(box);
  const toast = (k) => {
    const old = [...box.children].find((t) => t.dataset.k === k && !t._gone);
    if (old) { clearTimeout(old._h); old._h = setTimeout(() => out(old), 4500); return; }
    const t = document.createElement("div"); t.className = "atoast"; t.dataset.k = k;
    t.innerHTML = `<span><b>Just a preview.</b> In Echo, this ${SAY[k]}.</span><a href="/download">Get Echo →</a>`;
    box.appendChild(t); requestAnimationFrame(() => t.classList.add("on"));
    // max 2 at once. Count only notes that aren't already fading out: the old loop counted fading ones too, never
    // got below 3 and froze the page when the buttons were clicked quickly (chat 21)
    const live = [...box.children].filter((x) => !x._gone);
    live.slice(0, Math.max(0, live.length - 2)).forEach(out);
    t._h = setTimeout(() => out(t), 4500);
  };
  const out = (t) => { if (!t || t._gone) return; t._gone = true; t.classList.remove("on"); setTimeout(() => t.remove(), 220); };
  const menu = document.createElement("div"); menu.className = "mmenu"; menu.hidden = true;
  menu.innerHTML = '<button class="main" data-k="link">' + ICON.link + 'Copy share link</button>' +
    '<div class="chips"><button data-k="link">Never</button><button data-k="link">1 day</button><button data-k="link" class="on">7 days</button><button data-k="link">30 days</button></div><hr>' +
    '<p>Copy the video</p><button data-k="full">Full size</button><button data-k="free">Discord Free<small>shrinks to fit</small></button>' +
    '<button data-k="basic">Nitro Basic<small>shrinks to fit</small></button><button data-k="nitro">Nitro</button>';
  app.appendChild(menu);
  let owner = null;
  const closeMenu = () => { menu.hidden = true; if (owner) owner.classList.remove("menu"); owner = null; };
  app.querySelectorAll(".mclip").forEach((c) => {
    const act = c.querySelector(".mact"); if (!act) return;
    const copy = act.querySelector(".mcopy");   // like the app: the extra buttons slide in left of Copy
    // in one box that opens from no width (library.slint .more), so the name is squeezed smoothly instead of jumping
    const more = document.createElement("span"); more.className = "mmore";
    for (const k of ["edit", "link", "folder", "del"]) {
      const i = document.createElement("i"); i.className = "mx" + (k === "del" ? " del" : ""); i.dataset.k = k; i.title = k === "link" ? "Share link" : k[0].toUpperCase() + k.slice(1);
      i.innerHTML = ICON[k]; more.appendChild(i);
    }
    act.insertBefore(more, copy);
  });
  app.addEventListener("click", (e) => {
    const t = e.target;
    const ch = t.closest(".mch");
    if (ch) {
      const c = ch.closest(".mclip"); if (owner === c) return closeMenu();
      closeMenu(); owner = c; c.classList.add("menu");
      const r = ch.getBoundingClientRect(), a = app.getBoundingClientRect();
      menu.hidden = false;
      // the page may be zoomed (big screens: echo-head.js): screen positions are scaled, style.left/top are not
      const z = a.width / app.offsetWidth || 1, W = app.offsetWidth, H = app.offsetHeight;
      const left = Math.min((r.left - a.left) / z, W - menu.offsetWidth - 8);
      let top = (r.bottom - a.top) / z + 4; if (top + menu.offsetHeight > H - 6) top = (r.top - a.top) / z - menu.offsetHeight - 4;
      menu.style.left = Math.max(8, left) + "px"; menu.style.top = top + "px";
      return;
    }
    const star = t.closest(".mstar"); if (star) { star.classList.toggle("on"); return; }
    const k = t.closest("[data-k]");
    if (k && SAY[k.dataset.k]) { closeMenu(); return toast(k.dataset.k); }
    if (t.closest(".mcopy")) { closeMenu(); return toast("copy"); }
    const b = t.closest(".abtn"); if (b) { closeMenu(); return toast(b.classList.contains("pri") ? "save" : "rec"); }
    if (!t.closest(".mmenu")) closeMenu();
  });
  document.addEventListener("click", (e) => { if (!app.contains(e.target)) closeMenu(); });
})();

// ----- first visit to the homepage: a short, skippable "what makes Echo different" walk (chat 21). Shown once per
// browser (localStorage), never on phones, never if the visitor already scrolled. Scrolls to each highlight. -----
(() => {
  if (location.pathname !== "/" && !/\/index(\.html)?$/.test(location.pathname)) return;
  let seen = false; try { seen = !!localStorage.getItem("echo_tour"); } catch { return; }
  if (seen || innerWidth < 900 || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const STEPS = [
    ["#library .app", "Try it right here", "This is the real app layout - click around. One key saves the last moments; Copy ▾ shrinks a clip to fit Discord and copies it."],
    ["#features", "Everything after the moment", "Trim, edit, caption and share without another program. Share links play right inside Discord."],
    ["#light", "Tiny and light", "About 150 MB of memory while recording: a native Rust app with no bundled browser. Echo records in the background without costing you frames, even on low-end PCs."],
    ["#switch", "Bring your old clips", "Coming from Medal or another app? Echo finds your clips and copies them in, dates and game folders kept."],
    ["#storage", "Your clips, your storage", "Echo Cloud, or your own Google Drive, NAS or server for free. No ads, no account needed to clip."],
  ];
  const start = () => {
    try { localStorage.setItem("echo_tour", "1"); } catch {}
    const ov = document.createElement("div"); ov.className = "wtour";
    ov.innerHTML = '<div class="wthole"></div><div class="wtbub" role="dialog" aria-label="What makes Echo different"><span class="wtk">What makes Echo different</span><b></b><p></p><div class="wtbtns"><span class="wtn"></span><button class="wtskip">Skip</button><button class="wtnext">Next</button></div></div>';
    document.body.appendChild(ov);
    let i = -1, el = null;
    const hole = ov.querySelector(".wthole"), bub = ov.querySelector(".wtbub");
    const place = () => {
      if (!el) return;
      // big screens: echo-head.js zooms the page, so screen numbers are divided by the zoom for the fixed overlay
      const z = parseFloat(document.documentElement.style.zoom) || 1, innerWidth = window.innerWidth / z, innerHeight = window.innerHeight / z;
      const b = el.getBoundingClientRect(), r = { left: b.left / z, top: b.top / z, width: b.width / z, height: b.height / z, bottom: b.bottom / z }, pad = 10;
      Object.assign(hole.style, { left: r.left - pad + "px", top: Math.max(8, r.top - pad) + "px", width: r.width + pad * 2 + "px", height: Math.min(r.height + pad * 2, innerHeight - 16) + "px" });
      const bh = bub.offsetHeight, below = r.bottom + 18 + bh < innerHeight;
      // a big section: the bubble waits in the bottom-right corner instead of covering its heading
      if (r.height > innerHeight * 0.55) { bub.style.left = innerWidth - bub.offsetWidth - 24 + "px"; bub.style.top = innerHeight - bh - 24 + "px"; return; }
      bub.style.left = Math.max(16, Math.min(innerWidth - bub.offsetWidth - 16, r.left)) + "px";
      bub.style.top = (below ? r.bottom + 18 : Math.max(16, Math.min(innerHeight - bh - 16, r.top + 20))) + "px";
    };
    const end = () => { ov.remove(); removeEventListener("scroll", place); removeEventListener("resize", place); removeEventListener("keydown", key); };
    const next = () => {
      i++; if (i >= STEPS.length) return end();
      const [sel, t, p] = STEPS[i];
      el = document.querySelector(sel); if (!el) return next();
      bub.querySelector("b").textContent = t; bub.querySelector("p").textContent = p;
      bub.querySelector(".wtn").textContent = `${i + 1} / ${STEPS.length}`;
      bub.querySelector(".wtnext").textContent = i === STEPS.length - 1 ? "Get started" : "Next";
      el.scrollIntoView({ behavior: "smooth", block: el.offsetHeight > innerHeight * 0.7 ? "start" : "center" });
      setTimeout(place, 450); place();
    };
    const key = (e) => { if (e.key === "Escape") end(); else if (e.key === "ArrowRight" || e.key === "Enter") { e.preventDefault(); next(); } };
    ov.querySelector(".wtskip").onclick = end;
    ov.querySelector(".wtnext").onclick = () => { if (i === STEPS.length - 1) { end(); location.hash = "#download"; scrollTo({ top: 0, behavior: "smooth" }); } else next(); };
    // the page stays usable during the tour (step 2 says "click around" - a click used to close the tour);
    // Skip / Esc / the last step end it
    addEventListener("scroll", place, { passive: true }); addEventListener("resize", place); addEventListener("keydown", key);
    next();
  };
  // not on top of someone who's already reading: a small invite after a few seconds, only if they haven't scrolled
  setTimeout(() => {
    if (scrollY > 120) { try { localStorage.setItem("echo_tour", "1"); } catch {} return; }
    const inv = document.createElement("div"); inv.className = "wtinv";
    inv.innerHTML = '<span>New here? See what makes Echo different <i>(30 seconds)</i></span><button class="wtgo">Show me</button><button class="wtno" aria-label="No thanks">✕</button>';
    document.body.appendChild(inv);
    requestAnimationFrame(() => inv.classList.add("on"));
    const close = () => { inv.classList.remove("on"); setTimeout(() => inv.remove(), 300); };
    inv.querySelector(".wtgo").onclick = () => { close(); start(); };
    inv.querySelector(".wtno").onclick = () => { close(); try { localStorage.setItem("echo_tour", "1"); } catch {} };
  }, 2500);
})();

// chat 28: these used to be inline <script>s on index.html / pricing.html - the site's Content-Security-Policy
// (cloud/site/_headers) only runs scripts from files
// index: "Try it here / Real screenshot" switch
(function(){var sw=document.querySelector('.viewsw');if(!sw)return;var app=document.querySelector('.hero .app'),img=document.querySelector('.realshot');
sw.addEventListener('click',function(e){var b=e.target.closest('button');if(!b)return;var shot=b.dataset.view==='shot';
img.hidden=!shot;app.hidden=shot;sw.querySelectorAll('button').forEach(function(x){x.setAttribute('aria-selected',x===b)});});})();
// pricing: month / year toggle + live prices from Polar
if (document.querySelector(".plan .price[data-month]")) (() => {
  let interval = "month";
  const money = (n, cur) => new Intl.NumberFormat("en-IE", { style: "currency", currency: (cur || "eur").toUpperCase(),
    minimumFractionDigits: n % 1 ? 2 : 0 }).format(n);
  const show = () => {
    document.querySelectorAll(".toggle button").forEach((b) => b.classList.toggle("on", b.dataset.int === interval));
    document.querySelectorAll(".plan .price[data-month]").forEach((p) => {
      p.innerHTML = `${p.dataset[interval]} <small>/ ${interval}</small>`;
    });
  };
  document.querySelectorAll(".toggle button").forEach((b) => b.onclick = () => { interval = b.dataset.int; show(); });
  // live prices from Polar (same list Echo shows)
  fetch("/plans").then((r) => r.ok ? r.json() : null).then((j) => {
    if (!j || !j.plans) return;
    for (const p of j.plans) {
      const card = document.querySelector(`.plan[data-gb="${p.gb >= 1000 ? 1000 : p.gb}"] .price`);
      if (card && (p.interval === "month" || p.interval === "year")) card.dataset[p.interval] = money(p.price, p.currency);
    }
    show();
  }).catch(() => {});
})();
