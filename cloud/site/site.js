// echoclip.link: theme picker + drifting particles in the hero
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
