// Echo share page player: smooth, thick timeline (updated every frame like YouTube), buffered bar,
// hover time, play / volume / fullscreen, keyboard shortcuts. Without this file the browser's own player is used.
(() => {
  const v = document.querySelector(".player video");
  const box = document.querySelector(".player");
  if (!v || !box) return;
  v.controls = false;
  box.classList.add("ep");
  const I = {
    play: '<path d="M8 5.5v13l11-6.5z"/>',
    pause: '<path d="M7 5h3.5v14H7zM13.5 5H17v14h-3.5z"/>',
    vol: '<path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z"/><path class="w" d="M15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11"/>',
    mute: '<path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z"/><path class="w" d="M16 9.5l5 5M21 9.5l-5 5"/>',
    full: '<path class="w" d="M4 9V4h5M15 4h5v5M20 15v5h-5M9 20H4v-5"/>',
    exit: '<path class="w" d="M9 4v5H4M20 9h-5V4M15 20v-5h5M4 15h5v5"/>',
  };
  const svg = (k) => `<svg viewBox="0 0 24 24">${I[k]}</svg>`;
  const ui = document.createElement("div");
  ui.className = "epui";
  ui.innerHTML = `
    <div class="epbig">${svg("play")}</div>
    <div class="epbar">
      <div class="eptl" role="slider" aria-label="Seek" tabindex="0"><div class="eptrack"><i class="epbuf"></i><i class="epdone"></i></div><i class="epknob"></i><span class="eptip">0:00</span></div>
      <div class="eprow">
        <button class="epb" data-a="play" aria-label="Play">${svg("play")}</button>
        <div class="epvol"><button class="epb" data-a="mute" aria-label="Mute">${svg("vol")}</button><input type="range" min="0" max="1" step="0.02" value="1" aria-label="Volume"></div>
        <span class="eptime">0:00 / 0:00</span>
        <span class="epgrow"></span>
        <button class="epb" data-a="full" aria-label="Fullscreen">${svg("full")}</button>
      </div>
    </div>`;
  box.appendChild(ui);
  // Echo watermark: sits on the picture itself (not on black bars), same size/spot as the burned-in one,
  // and its 1 s animation starts when the video does
  const wm = box.querySelector(".wm");
  if (wm) {
    const src = wm.getAttribute("src");
    wm.removeAttribute("src");
    wm.style.visibility = "hidden";
    const place = () => {
      const bw = box.clientWidth, bh = box.clientHeight, vw = v.videoWidth, vh = v.videoHeight;
      if (!vw || !vh || !bw || !bh) return;
      const vb = v.getBoundingClientRect(), bb = box.getBoundingClientRect();
      const s = Math.min(vb.width / vw, vb.height / vh), pw = vw * s, ph = vh * s;
      const left = vb.left - bb.left + (vb.width - pw) / 2, top = vb.top - bb.top + (vb.height - ph) / 2;
      wm.style.maxHeight = "none";
      wm.style.height = Math.max(12, ph * 0.075) + "px";
      wm.style.right = bw - (left + pw) + ph * 0.025 + "px";
      wm.style.bottom = bh - (top + ph) + ph * 0.025 + "px";
    };
    let shown = false;
    const show = () => { if (shown) return; shown = true; place(); wm.src = src; wm.style.visibility = ""; };
    v.addEventListener("loadedmetadata", place);
    v.addEventListener("playing", show);
    v.addEventListener("pause", () => { if (!shown) show(); });
    addEventListener("resize", place);
    document.addEventListener("fullscreenchange", () => setTimeout(place, 50));
    document.addEventListener("webkitfullscreenchange", () => setTimeout(place, 50));
    if (window.ResizeObserver) new ResizeObserver(place).observe(box);
    setTimeout(show, 4000);   // autoplay blocked and nobody pressed play: show it anyway
  }
  const $ = (s) => ui.querySelector(s);
  const tl = $(".eptl"), done = $(".epdone"), buf = $(".epbuf"), knob = $(".epknob"), tip = $(".eptip");
  const bPlay = $('[data-a="play"]'), bMute = $('[data-a="mute"]'), bFull = $('[data-a="full"]'), vol = $(".epvol input"), time = $(".eptime"), big = $(".epbig");
  const fmt = (t) => { t = Math.max(0, t || 0); const m = Math.floor(t / 60), s = Math.floor(t % 60); return `${m}:${String(s).padStart(2, "0")}`; };
  const dur = () => (isFinite(v.duration) ? v.duration : 0);
  // phones/tablets: tap shows/hides the controls (like YouTube), the middle button plays/pauses
  let touch = matchMedia("(hover: none)").matches;
  const setTouch = (t) => { touch = t; box.classList.toggle("touch", t); };
  setTouch(touch);
  box.addEventListener("pointerdown", (e) => setTouch(e.pointerType === "touch" || e.pointerType === "pen"), true);

  // ---- smooth timeline: redraw every animation frame while playing (timeupdate only fires ~4x a second)
  let dragging = false, dragT = 0;
  const draw = () => {
    const d = dur(), t = dragging ? dragT : v.currentTime, p = d ? Math.min(1, t / d) : 0;
    done.style.transform = `scaleX(${p})`;
    knob.style.left = p * 100 + "%";
    time.textContent = `${fmt(t)} / ${fmt(d)}`;
    if (v.buffered.length && d) buf.style.transform = `scaleX(${Math.min(1, v.buffered.end(v.buffered.length - 1) / d)})`;
  };
  let raf = 0;
  const loop = () => { draw(); raf = v.paused ? 0 : requestAnimationFrame(loop); };
  const kick = () => { if (!raf) raf = requestAnimationFrame(loop); };
  ["play", "playing", "seeked", "loadedmetadata", "progress", "timeupdate", "durationchange"].forEach((e) => v.addEventListener(e, () => { draw(); if (!v.paused) kick(); }));

  // ---- play / pause
  const syncPlay = () => {
    bPlay.innerHTML = svg(v.paused ? "play" : "pause");
    bPlay.setAttribute("aria-label", v.paused ? "Play" : "Pause");
    box.classList.toggle("paused", v.paused);
    big.innerHTML = svg(v.paused ? "play" : "pause");
  };
  v.addEventListener("play", syncPlay); v.addEventListener("pause", syncPlay); v.addEventListener("ended", syncPlay);
  const toggle = () => (v.paused ? v.play().catch(() => {}) : v.pause());
  bPlay.onclick = toggle;
  big.onclick = (e) => { e.stopPropagation(); toggle(); wake(); };
  v.addEventListener("click", () => {
    if (!touch) return toggle();
    if (box.classList.contains("idle") || v.paused) wake();   // tap: show the controls
    else { clearTimeout(idle); box.classList.add("idle"); }   // tap again: hide them
  });
  v.addEventListener("dblclick", () => { if (!touch) fullscreen(); });

  // ---- seeking: click or drag anywhere on the (thick) bar
  const at = (e) => { const r = tl.getBoundingClientRect(); return Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) * dur(); };
  tl.addEventListener("pointerdown", (e) => {
    if (!dur()) return;
    dragging = true; tl.setPointerCapture(e.pointerId); box.classList.add("seeking");
    const wasPlaying = !v.paused; v.pause();
    dragT = at(e); v.currentTime = dragT; draw();
    const mv = (ev) => { dragT = at(ev); v.currentTime = dragT; draw(); };
    const up = () => {
      dragging = false; box.classList.remove("seeking");
      tl.removeEventListener("pointermove", mv); tl.removeEventListener("pointerup", up); tl.removeEventListener("pointercancel", up);
      if (wasPlaying) v.play().catch(() => {});
      draw();
    };
    tl.addEventListener("pointermove", mv); tl.addEventListener("pointerup", up); tl.addEventListener("pointercancel", up);
  });
  tl.addEventListener("pointermove", (e) => {   // time under the mouse
    const r = tl.getBoundingClientRect(), x = Math.min(r.width, Math.max(0, e.clientX - r.left));
    tip.textContent = fmt(at(e)); tip.style.left = x + "px";
  });

  // ---- volume (remembered in this browser)
  try { const s = JSON.parse(localStorage.getItem("ep_vol") || "null"); if (s) { v.volume = s.v; v.muted = s.m; } } catch (e) {}
  const syncVol = () => {
    vol.value = v.muted ? 0 : v.volume;
    bMute.innerHTML = svg(v.muted || v.volume === 0 ? "mute" : "vol");
    vol.style.setProperty("--p", (v.muted ? 0 : v.volume) * 100 + "%");
    try { localStorage.setItem("ep_vol", JSON.stringify({ v: v.volume, m: v.muted })); } catch (e) {}
  };
  v.addEventListener("volumechange", syncVol);
  vol.oninput = () => { v.volume = +vol.value; v.muted = v.volume === 0; };
  bMute.onclick = () => { v.muted = !v.muted; if (!v.muted && v.volume === 0) v.volume = 0.6; };

  // ---- fullscreen
  // iPhone Safari can't put a <div> in fullscreen, only the <video> itself (then iOS shows its own player)
  const fsEl = () => document.fullscreenElement || document.webkitFullscreenElement;
  const fullscreen = () => {
    try {
      let r;
      if (fsEl()) r = (document.exitFullscreen || document.webkitExitFullscreen).call(document);
      else if (box.requestFullscreen) r = box.requestFullscreen({ navigationUI: "hide" });
      else if (box.webkitRequestFullscreen) r = box.webkitRequestFullscreen();
      else if (v.webkitEnterFullscreen) r = v.webkitEnterFullscreen();
      if (r && r.then) r.then(() => {
        // phones: turn sideways for wide clips (Android Chrome; ignored elsewhere)
        if (touch && fsEl() && v.videoWidth > v.videoHeight && screen.orientation && screen.orientation.lock) screen.orientation.lock("landscape").catch(() => {});
      }).catch(() => {});
    } catch (e) {}
  };
  bFull.onclick = fullscreen;
  const fsChange = () => {
    bFull.innerHTML = svg(fsEl() ? "exit" : "full");
    if (!fsEl() && screen.orientation && screen.orientation.unlock) try { screen.orientation.unlock(); } catch (e) {}
  };
  document.addEventListener("fullscreenchange", fsChange);
  document.addEventListener("webkitfullscreenchange", fsChange);

  // ---- controls fade out while playing and the mouse rests
  let idle = 0;
  const wake = () => { box.classList.remove("idle"); clearTimeout(idle); idle = setTimeout(() => { if (!v.paused && !dragging) box.classList.add("idle"); }, touch ? 3000 : 2200); };
  // mouse only: on phones a tap also sends move/leave events, which would hide the controls straight away
  box.addEventListener("pointermove", (e) => { if (e.pointerType === "mouse") wake(); });
  box.addEventListener("pointerleave", (e) => { if (e.pointerType === "mouse" && !v.paused) box.classList.add("idle"); });
  ui.querySelector(".epbar").addEventListener("pointerdown", () => wake());   // using the bar keeps it visible
  v.addEventListener("play", wake);

  // ---- keyboard: Space/K play, ←/→ 5 s, J/L 10 s, ↑/↓ volume, F fullscreen, M mute, 0-9 jump
  document.addEventListener("keydown", (e) => {
    if (e.ctrlKey || e.metaKey || e.altKey) return;   // Ctrl+F (find), Ctrl+K … stay the browser's
    if (e.target.closest && e.target.closest("input,textarea") && e.target !== vol) return;
    const k = e.key.toLowerCase(), d = dur();
    if (k === " " || k === "k") { e.preventDefault(); toggle(); }
    else if (k === "arrowleft") { e.preventDefault(); v.currentTime = Math.max(0, v.currentTime - 5); }
    else if (k === "arrowright") { e.preventDefault(); v.currentTime = Math.min(d, v.currentTime + 5); }
    else if (k === "j") v.currentTime = Math.max(0, v.currentTime - 10);
    else if (k === "l") v.currentTime = Math.min(d, v.currentTime + 10);
    else if (k === "arrowup") { e.preventDefault(); v.muted = false; v.volume = Math.min(1, v.volume + 0.1); }
    else if (k === "arrowdown") { e.preventDefault(); v.volume = Math.max(0, v.volume - 0.1); }
    else if (k === "f") fullscreen();
    else if (k === "m") v.muted = !v.muted;
    else if (/^[0-9]$/.test(k) && d) v.currentTime = d * (+k / 10);
    else return;
    wake(); draw();
  });

  syncPlay(); syncVol(); draw();
  if (v.autoplay) v.play().catch(() => { syncPlay(); });   // browsers may block autoplay with sound: then the big play button shows
})();
