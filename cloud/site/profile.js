// echoclip.link/u/<name>: "Follow" explains how to follow in Echo (the website has no sign-in)
(() => {
  const b = document.getElementById("fbtn"), n = document.getElementById("note");
  if (!b || !n) return;
  let h;
  b.addEventListener("click", () => {
    n.classList.add("on"); clearTimeout(h); h = setTimeout(() => n.classList.remove("on"), 7000);
    try { navigator.clipboard.writeText("@" + b.dataset.h).catch(() => {}); } catch { /* fine */ }
  });
})();
