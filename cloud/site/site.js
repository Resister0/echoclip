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
  if (linux && win && lin) { dl.insertBefore(lin, win); lin.classList.add("primary"); win.classList.remove("primary"); }
  // /download: the latest version under the buttons
  const dv = document.getElementById("dlver");
  if (dv) fetch("/changes").then((r) => r.json()).then((j) => {
    const x = (j.releases || [])[0]; if (!x) return;
    const d = new Date(x.at * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
    dv.innerHTML = `Echo ${String(x.version).replace(/[^0-9.]/g, "")} · ${d} · Windows 10/11 and Linux · <a href="/changelog">What's new</a>`;
  }).catch(() => {});
  // /changelog: every published version (from the server's list of releases)
  const box = document.getElementById("changes");
  if (box) fetch("/changes").then((r) => r.json()).then((j) => {
    const esc = (t) => String(t).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
    const day = (t) => new Date(t * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
    const list = j.releases || [];
    box.innerHTML = list.length ? list.map((x, i) => `<div class="chg"><b>${esc(x.version)}${i === 0 ? ' <span class="new">Latest</span>' : ""}</b><p>${esc(x.notes)}</p><time>${day(x.at)}</time></div>`).join("")
      : '<p class="sub">No versions listed yet.</p>';
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
    const old = [...box.children].find((t) => t.dataset.k === k);
    if (old) { clearTimeout(old._h); old._h = setTimeout(() => out(old), 4500); return; }
    const t = document.createElement("div"); t.className = "atoast"; t.dataset.k = k;
    t.innerHTML = `<span><b>Just a preview.</b> In Echo, this ${SAY[k]}.</span><a href="/download">Get Echo →</a>`;
    box.appendChild(t); requestAnimationFrame(() => t.classList.add("on"));
    while (box.children.length > 2) out(box.firstChild);
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
    const star = act.querySelector(".mstar");
    for (const k of ["edit", "link", "folder", "del"]) {
      const i = document.createElement("i"); i.className = "mx" + (k === "del" ? " del" : ""); i.dataset.k = k; i.title = k === "link" ? "Share link" : k[0].toUpperCase() + k.slice(1);
      i.innerHTML = ICON[k]; act.insertBefore(i, star);
    }
  });
  app.addEventListener("click", (e) => {
    const t = e.target;
    const ch = t.closest(".mch");
    if (ch) {
      const c = ch.closest(".mclip"); if (owner === c) return closeMenu();
      closeMenu(); owner = c; c.classList.add("menu");
      const r = ch.getBoundingClientRect(), a = app.getBoundingClientRect();
      menu.hidden = false;
      const left = Math.min(r.left - a.left, a.width - menu.offsetWidth - 8);
      let top = r.bottom - a.top + 4; if (top + menu.offsetHeight > a.height - 6) top = r.top - a.top - menu.offsetHeight - 4;
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
