// echoclip.link/mod (chat 29): moderators sign in with their Echo Cloud email (6-digit code) and handle reports.
// Only accounts the developer added in /admin get a code. Session token lives in this tab only (sessionStorage).
const $ = (s) => document.querySelector(s);
const esc = (t) => String(t ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const ago = (t) => { if (!t) return "-"; const d = Date.now() / 1000 - t; return d < 3600 ? Math.max(1, Math.round(d / 60)) + " min ago" : d < 86400 ? Math.round(d / 3600) + " h ago" : Math.round(d / 86400) + " days ago"; };
// signed in on /admin? that session works here too (chat 30)
let token = ""; try { token = sessionStorage.getItem("echo_mod_token") || localStorage.getItem("echo_admin_token") || ""; } catch { }
let email = "";

async function api(method, path, b) {
  const h = { "content-type": "application/json" }; if (token) h.authorization = "Bearer " + token;
  const r = await fetch(path, { method, headers: h, body: b ? JSON.stringify(b) : undefined });
  const x = await r.json().catch(() => ({}));
  return { ok: r.ok, status: r.status, x };
}

function login(msg) {
  $("#app").innerHTML = `<div class="login"><h1>Echo moderation</h1>
    <div class="sub">Sign in with the email of your Echo Cloud account. Only moderators get a code.</div>
    <input id="em" type="email" placeholder="you@example.com" autocomplete="email" value="${esc(email)}"><button id="go">Send code</button>
    <div class="err" id="err">${esc(msg || "")}</div></div>`;
  const send = async () => {
    email = $("#em").value.trim(); if (!email) return;
    $("#err").textContent = "…";
    let r; try { r = await api("POST", "/auth/start", { email, mod: true }); } catch { $("#err").textContent = "Couldn't reach echoclip.link"; return; }
    if (!r.ok) { $("#err").textContent = r.x.error || "Error " + r.status; return; }
    code();
  };
  $("#go").onclick = send; $("#em").onkeydown = (e) => { if (e.key === "Enter") send(); }; $("#em").focus();
}
function code(msg) {
  $("#app").innerHTML = `<div class="login"><h1>Check your email</h1>
    <div class="sub">If ${esc(email)} is a moderator, a 6-digit code is on its way (it works for 10 minutes).</div>
    <input id="cd" inputmode="numeric" autocomplete="one-time-code" placeholder="123456" maxlength="6"><button id="go">Sign in</button> <button id="back">Other email</button>
    <div class="err" id="err">${esc(msg || "")}</div></div>`;
  const go = async () => {
    $("#err").textContent = "…";
    let r; try { r = await api("POST", "/auth/verify", { email, code: $("#cd").value.trim(), mod: true }); } catch { $("#err").textContent = "Couldn't reach echoclip.link"; return; }
    if (!r.ok || !r.x.token) { $("#err").textContent = r.x.error || "Error " + r.status; return; }
    token = r.x.token; try { sessionStorage.setItem("echo_mod_token", token); } catch { }
    load();
  };
  $("#go").onclick = go; $("#cd").onkeydown = (e) => { if (e.key === "Enter") go(); }; $("#back").onclick = () => login(); $("#cd").focus();
}
function forget() { if (token.startsWith("adm_")) { try { localStorage.removeItem("echo_admin_token"); } catch { } } token = ""; try { sessionStorage.removeItem("echo_mod_token"); } catch { } }

async function load() {
  if (!token) return login();
  let r; try { r = await api("GET", "/mod/me"); } catch { return login("Couldn't reach echoclip.link"); }
  if (r.status === 401 || r.status === 403) { forget(); return login(r.x.error || "Signed out"); }
  if (!r.ok) return login(r.x.error || "Error " + r.status);
  draw(r.x);
}

function report(o) {
  const link = esc(o.kind === "s" ? `/s/${o.target}` : `/u/${o.target}`);
  const name = o.kind === "s" ? esc(o.title || "Shared clip") + ` <span class="sub">/s/${esc(o.target)}</span>` : `@${esc(o.target)}${o.title ? ` <span class="sub">${esc(o.title)}</span>` : ""}`;
  const items = o.items.map((i) => `<li><b>${esc(i.reason)}</b> · ${ago(i.created)}${i.details ? ` - ${esc(i.details)}` : ""}</li>`).join("");
  const acts = !o.exists ? `<span class="gone">Already gone</span> <button data-a="dismiss">Close</button>`
    : o.kind === "s" ? `<button class="danger" data-a="rmshare">Remove link</button><button data-a="dismiss">Nothing wrong</button>`
    : `<button class="danger" data-a="rmprof">Remove profile</button><label class="sub"><input type="checkbox" class="blk" checked> block the @name</label>
       <button data-a="rename">Random @name</button><button data-a="dismiss">Nothing wrong</button>`;
  return `<div class="rep" data-k="${o.kind}" data-t="${esc(o.target)}"><div class="hd"><span class="pill bad">${o.n} report${o.n === 1 ? "" : "s"}</span>
      <a href="${link}" target="_blank" rel="noopener">${name}</a><span class="sp"></span><span class="sub">${o.kind === "s" ? "clip" : "profile"} · last ${ago(o.last)}</span></div>
    ${o.bio ? `<div class="sub" style="margin-top:6px">Bio: ${esc(o.bio)}</div>` : ""}<ul>${items}</ul><div class="act">${acts}<span class="sub msg"></span></div></div>`;
}

function draw(j) {
  $("#app").innerHTML = `
    <div class="top"><h1>Echo moderation</h1><span class="sub">signed in as ${esc(j.name)}</span><span class="sp"></span>
      <button id="re">Refresh</button><button id="out">Sign out</button></div>
    <h2>Open reports</h2>
    ${j.open.length ? j.open.map(report).join("") : `<div class="tile sub">No open reports. Nice.</div>`}
    <h2>Do it by hand</h2>
    <div class="two">
      <div class="tile"><div class="k">Remove a share link</div>
        <input id="rmshare" placeholder="https://echoclip.link/s/…" style="width:100%;margin:8px 0">
        <div class="sub">The link stops working. The clip stays in its owner's Echo Cloud.</div>
        <div style="margin-top:10px"><button id="rmsharego" class="danger">Remove link</button> <span class="sub" id="rmsharemsg"></span></div></div>
      <div class="tile"><div class="k">Remove a profile</div>
        <input id="rmprof" placeholder="@name or https://echoclip.link/u/…" style="width:100%;margin:8px 0">
        <label class="sub"><input type="checkbox" id="rmblock" checked> block the @name so nobody can take it again</label>
        <div style="margin-top:10px"><button id="rmprofgo" class="danger">Remove profile</button> <span class="sub" id="rmprofmsg"></span></div></div>
    </div>
    <div class="tile" style="margin-top:10px"><div class="k">Change an @name</div>
      <div style="display:flex;gap:8px;flex-wrap:wrap;margin:8px 0">
        <input id="hwho" placeholder="their @name or /u/ link" style="flex:1;min-width:200px">
        <input id="hto" placeholder="new @name (empty = random user_123456)" style="flex:1;min-width:200px"></div>
      <label class="sub"><input type="checkbox" id="hblock" checked> block the old @name (inappropriate names)</label>
      <div style="margin-top:10px"><button id="hgo">Change @name</button> <span class="sub" id="hmsg"></span></div></div>
    <h2>Recent actions</h2>
    <div class="wrap"><table><tr><th>When</th><th>Who</th><th>What</th><th>On</th></tr>
      ${j.log.length ? j.log.map((l) => `<tr><td>${ago(l.at)}</td><td>${esc(l.who)}</td><td>${esc(l.action)}</td><td>${esc(l.target)}</td></tr>`).join("") : `<tr><td colspan="4" class="sub">Nothing yet</td></tr>`}</table></div>
    <div class="note">Removing something closes its open reports. Everything done here is listed under Recent actions and visible to the developer.
      Deleting clips or accounts is only possible for the developer - write to support@echoclip.link if a clip itself has to go.</div>`;
  $("#re").onclick = load;
  $("#out").onclick = async () => { try { await api("POST", "/auth/logout"); } catch { } forget(); login(); };
  const run = async (path, b, out, okText) => {
    out.textContent = "…";
    try { const r = await api("POST", path, b); if (r.ok) { out.textContent = okText(r.x); setTimeout(load, 900); } else out.textContent = r.x.error || "Error " + r.status; }
    catch { out.textContent = "Couldn't reach echoclip.link"; }
  };
  document.querySelectorAll(".rep button").forEach((btn) => btn.onclick = () => {
    const box = btn.closest(".rep"), k = box.dataset.k, t = box.dataset.t, out = box.querySelector(".msg"), a = btn.dataset.a;
    if (a === "dismiss") return run("/mod/dismiss", { kind: k, target: t }, out, () => "Closed");
    if (a === "rmshare" && confirm("Remove this share link?")) return run("/mod/takedown", { id: t }, out, () => "Link removed");
    if (a === "rmprof" && confirm(`Remove @${t}'s profile?`)) return run("/mod/profile-takedown", { handle: t, block: box.querySelector(".blk").checked }, out, () => "Profile removed");
    if (a === "rename" && confirm(`Give @${t} a random @name (and block @${t})?`)) return run("/mod/profile-handle", { who: t, to: "", block_old: true }, out, (x) => `Now @${x.to}`);
  });
  $("#rmsharego").onclick = () => { const v = $("#rmshare").value.trim(); if (v && confirm("Remove this share link?")) run("/mod/takedown", { id: v }, $("#rmsharemsg"), () => "Link removed"); };
  $("#rmprofgo").onclick = () => { const v = $("#rmprof").value.trim(); if (v && confirm("Remove this profile?")) run("/mod/profile-takedown", { handle: v, block: $("#rmblock").checked }, $("#rmprofmsg"), () => "Profile removed"); };
  $("#hgo").onclick = () => {
    const who = $("#hwho").value.trim(), to = $("#hto").value.trim();
    if (who && confirm(to ? `Change ${who} to @${to.replace(/^@/, "")}?` : `Give ${who} a random @name?`))
      run("/mod/profile-handle", { who, to, block_old: $("#hblock").checked }, $("#hmsg"), (x) => `Done: @${x.from} → @${x.to}`);
  };
}
load();
