// ----- clip pictures (12 Nov 2026) -----
// A clip that's only in Echo Cloud (not on this PC) showed as a black card in Echo's Library: the picture lived only in
// <clips>/.thumbs/<name>.jpg on the PC that recorded it. Now each PC sends the card picture of its clips that are in
// Echo Cloud once (the same 480 px JPEG the card shows, ~15-40 KB), and a PC that lists a cloud-only clip fetches it
// into its own .thumbs folder. One per clip: D1 row clip_thumbs (key = the clip's R2 key, id = its picture
// cthumbs/<user>/<id>.jpg in R2, hash, length). Goes with the clip (/delete, the daily jobs, takedown), follows /move,
// goes with the account. Only for clips that are in R2 (head) - no pictures of clips that don't exist.
// Older Echo never calls these routes; older uploads get their picture from the PC that has the clip.

const MAX_JPG = 128 * 1024;   // bytes (Echo's card pictures are 480 px wide)
const PUTS_PER_HOUR = 1200;   // a PC with many old backups fills them in over a few rounds
const now = () => Math.floor(Date.now() / 1000);
const json = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const fail = (msg, status = 400) => json({ error: msg }, status);
const r2Key = (userId, id) => `cthumbs/${userId}/${id}.jpg`;

export const CLIP_THUMB_TABLES = [
  "CREATE TABLE IF NOT EXISTS clip_thumbs (key TEXT PRIMARY KEY, user_id TEXT NOT NULL, id TEXT NOT NULL, hash TEXT NOT NULL, dur REAL, updated INTEGER NOT NULL)",
  "CREATE INDEX IF NOT EXISTS clip_thumbs_user ON clip_thumbs (user_id)",
];

function b64(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

async function hex(bytes) {
  const d = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(d)].slice(0, 16).map((b) => b.toString(16).padStart(2, "0")).join("");
}

/// the paths Echo sent (["Echo/…/clip.mp4"]) -> [[path, R2 key]] (bad paths dropped), at most `max`
function keysOf(lib, user, paths, max) {
  const seen = new Set(), out = [];
  for (const p of Array.isArray(paths) ? paths : []) {
    const k = lib.userKey(user, p);
    if (!k || seen.has(k)) continue;
    seen.add(k); out.push([String(p).replace(/^\/+/, ""), k]);
    if (out.length >= max) break;
  }
  return out;
}

async function rowsOf(env, userId, keys, cols) {
  const out = [];
  for (let i = 0; i < keys.length; i += 90) {
    const part = keys.slice(i, i + 90);
    const r = (await env.DB.prepare(`SELECT ${cols} FROM clip_thumbs WHERE user_id = ? AND key IN (${part.map(() => "?").join(",")})`)
      .bind(userId, ...part).all()).results || [];
    out.push(...r);
  }
  return out;
}

/// POST /thumbs/have {paths} -> {thumbs: {path: hash}} - which of these clips have a picture (up to 1000 a call)
export async function thumbsHave(req, user, env, lib) {
  const b = await lib.body(req);
  const pk = keysOf(lib, user, b.paths, 1000), byKey = new Map(pk.map(([p, k]) => [k, p]));
  const thumbs = {};
  for (const r of await rowsOf(env, user.id, pk.map((x) => x[1]), "key, hash")) thumbs[byKey.get(r.key)] = r.hash;
  return json({ thumbs });
}

/// POST /thumbs/get {paths} -> {thumbs: {path: {jpg: base64, hash, dur}}} (up to 40 a call; clips without one are left out)
export async function thumbsGet(req, user, env, lib) {
  const b = await lib.body(req);
  const pk = keysOf(lib, user, b.paths, 40), byKey = new Map(pk.map(([p, k]) => [k, p]));
  const rows = await rowsOf(env, user.id, pk.map((x) => x[1]), "key, id, hash, dur");
  const thumbs = {}, lost = [];
  await Promise.all(rows.map(async (r) => {
    const o = await env.BUCKET.get(r2Key(user.id, r.id));
    if (!o) { lost.push(r.key); return; }
    thumbs[byKey.get(r.key)] = { jpg: b64(new Uint8Array(await o.arrayBuffer())), hash: r.hash, dur: r.dur || 0 };
  }));
  // a row whose picture is gone (shouldn't happen): forgotten, so a PC that has the clip sends it again
  for (const k of lost) await env.DB.prepare("DELETE FROM clip_thumbs WHERE key = ? AND user_id = ?").bind(k, user.id).run();
  return json({ thumbs });
}

/// POST /thumbs/put {path, jpg: base64, dur}: the card picture of clip `path` (must be in Echo Cloud). Same picture = no write.
export async function thumbsPut(req, user, env, lib) {
  const b = await lib.body(req);
  const key = lib.userKey(user, b.path);
  if (!key || key.includes("/Echo/.")) return fail("Bad clip path");
  const s = String(b.jpg || "").replace(/\s+/g, "");
  if (!s || s.length > Math.ceil(MAX_JPG / 3) * 4 || !/^[A-Za-z0-9+/]+={0,2}$/.test(s)) return fail("Bad picture");
  let bytes;
  try { bytes = Uint8Array.from(atob(s), (c) => c.charCodeAt(0)); } catch { return fail("Bad picture"); }
  if (bytes.length < 100 || bytes[0] !== 0xff || bytes[1] !== 0xd8 || bytes[2] !== 0xff) return fail("Bad picture");
  const hash = await hex(bytes);
  const dur = Number(b.dur) > 0 && Number(b.dur) < 1e6 ? Number(b.dur) : null;
  const old = await env.DB.prepare("SELECT id, hash FROM clip_thumbs WHERE key = ? AND user_id = ?").bind(key, user.id).first();
  if (old && old.hash === hash) return json({ ok: true, hash, same: true });
  // the clip itself is in Echo Cloud (or a share-first upload of it is: its original sits under a hidden name)
  if (!(await env.BUCKET.head(key)) && !(await env.DB.prepare("SELECT 1 FROM swaps WHERE key = ? AND user_id = ?").bind(key, user.id).first()))
    return fail("That clip isn't in Echo Cloud", 404);
  if (!(await lib.allow(env, `cthumb:${user.id}`, PUTS_PER_HOUR, 3600))) return fail("Too many pictures - try again later", 429);
  const id = old ? old.id : lib.randomId() + lib.randomId();
  await env.BUCKET.put(r2Key(user.id, id), bytes, { httpMetadata: { contentType: "image/jpeg" } });
  await env.DB.prepare(`INSERT INTO clip_thumbs (key, user_id, id, hash, dur, updated) VALUES (?, ?, ?, ?, ?, ?)
    ON CONFLICT(key) DO UPDATE SET user_id = excluded.user_id, id = excluded.id, hash = excluded.hash, dur = excluded.dur, updated = excluded.updated`)
    .bind(key, user.id, id, hash, dur, now()).run();
  return json({ ok: true, hash });
}

/// the clips `keys` (R2 keys) of user `userId` are gone: their pictures go too
export async function dropClipThumbs(env, userId, keys) {
  if (!keys || !keys.length) return;
  const rows = await rowsOf(env, userId, keys, "key, id");
  if (!rows.length) return;
  for (let i = 0; i < rows.length; i += 1000) await env.BUCKET.delete(rows.slice(i, i + 1000).map((r) => r2Key(userId, r.id)));
  for (let i = 0; i < rows.length; i += 90) {
    const part = rows.slice(i, i + 90).map((r) => r.key);
    await env.DB.prepare(`DELETE FROM clip_thumbs WHERE user_id = ? AND key IN (${part.map(() => "?").join(",")})`).bind(userId, ...part).run();
  }
}

/// clip `from` was renamed / moved to `to` (R2 keys): its picture follows (a picture `to` had before goes)
export async function moveClipThumb(env, userId, from, to) {
  if (from === to) return;
  await dropClipThumbs(env, userId, [to]);
  await env.DB.prepare("UPDATE clip_thumbs SET key = ? WHERE key = ? AND user_id = ?").bind(to, from, userId).run();
}

/// account deleted: every picture of it (several calls for a huge account are fine: what's left goes next time)
export async function dropUserThumbs(env, userId) {
  for (let round = 0; round < 20; round++) {
    const l = await env.BUCKET.list({ prefix: `cthumbs/${userId}/`, limit: 1000 });
    if (!l.objects.length) break;
    await env.BUCKET.delete(l.objects.map((o) => o.key));
    if (!l.truncated) break;
  }
  await env.DB.prepare("DELETE FROM clip_thumbs WHERE user_id = ?").bind(userId).run();
}
