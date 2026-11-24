// echoclip.link/status (21 Nov 2026): a live check from the visitor's browser - /health answers = Echo Cloud and the
// share links work; the release list answers = updates work (never /update/..., which counts PCs). Nothing is stored or sent anywhere else.
(function () {
  const set = (id, ok, good, bad) => {
    const el = document.getElementById(id);
    if (!el) return;
    el.textContent = (ok ? "✓ " : "✗ ") + (ok ? good : bad);
    el.style.color = ok ? "" : "#e5484d";
  };
  const t0 = Date.now();
  fetch("/health", { cache: "no-store" })
    .then((r) => (r.ok ? r.json() : Promise.reject(r.status)))
    .then((j) => {
      const ms = Date.now() - t0;
      set("st-cloud", !!j.ok, `Working - answered in ${ms} ms.`, "Not answering properly right now.");
      set("st-share", !!j.ok, "Working - links play in Discord and in the browser.", "May not open right now.");
    })
    .catch(() => {
      set("st-cloud", false, "", "Not reachable right now. We're probably on it already - it usually comes back within minutes.");
      set("st-share", false, "", "May not open right now.");
    });
  // the release list (NOT /update/...: that one is counted as an Echo PC checking in)
  fetch("/changes", { cache: "no-store" })
    .then((r) => set("st-upd", r.ok, "Working - Echo finds new versions by itself.", "The update check has trouble right now - Echo keeps working, it just won't see new versions for a while."))
    .catch(() => set("st-upd", false, "", "The update check has trouble right now - Echo keeps working, it just won't see new versions for a while."));
  const at = document.getElementById("st-at");
  if (at) at.textContent = "Checked " + new Date().toLocaleString();
})();
