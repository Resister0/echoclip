// Echo Cloud server: sign in with an email code, usage / quota, and short-lived R2 keys that only
// reach the user's own folder (u/<id>/) in the bucket. Echo then talks to R2 directly (rclone s3).
//
// Secrets (npx wrangler secret put NAME):
//   CF_API_TOKEN           Cloudflare API token with "Workers R2 Storage: Edit" (makes the temporary keys)
//   ACCOUNT_ID             Cloudflare account id
//   PARENT_ACCESS_KEY_ID   access key id of an R2 API token (Object Read & Write on echo-cloud)
//   RESEND_KEY             Resend API key for the sign-in emails (without it the code is only logged: wrangler tail)
//   POLAR_TOKEN            Polar organization access token (products:read, checkouts:write, customer_sessions:write)
//   POLAR_WEBHOOK_SECRET   secret of the Polar webhook pointing at https://echoclip.link/polar/webhook
//   RELEASE_KEY            (optional) long random string; the public build sends it with POST /release/push
//   ADMIN_KEY              (optional) long random string for /admin/takedown (removing reported share links)
// Var UPLOAD_KEYS: "rw" (default) = /creds hands out read-write keys like before; "ro" = read-only keys, and Echo
// uploads / deletes / moves through /upload, /delete, /move (per-file, size-checked). Switch to "ro" once every
// Echo in use has updated (chat 15+): then nobody can upload past their plan with the keys.
// Plans are the Polar products with metadata quota_gb; paying sets the user's quota, ending it goes back to free.

const GB = 1e9; // decimal GB, like Echo shows sizes
const PLANS = { free: 10 * GB, plus: 250 * GB, pro: 1024 * GB };
const CODE_MINUTES = 10;
const CREDS_SECONDS = 3600;     // short: upload keys can't be taken back, so an account can only overshoot its plan this long
const USAGE_CACHE_SECONDS = 300;
const USAGE_FRESH_SECONDS = 20; // "fresh" usage is still cached this long (listing the bucket costs money)
const SESSION_DAYS = 180;       // signed out after this long without using Echo Cloud
const OVER_GRACE_DAYS = 30;     // over the plan (e.g. a paid plan ended): clips above it are removed after this many days
const OVER_REMIND_DAYS = 23;    // reminder email (7 days before)
const OVER_DELETE_PER_RUN = 1000; // files removed per account per daily run (big accounts finish over several days)

const now = () => Math.floor(Date.now() / 1000);
const json = (body, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const fail = (msg, status = 400) => json({ error: msg }, status);

async function sha256(s) {
  const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s));
  return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("");
}
function randomHex(bytes) {
  return [...crypto.getRandomValues(new Uint8Array(bytes))].map((b) => b.toString(16).padStart(2, "0")).join("");
}
function randomId() {
  const abc = "abcdefghijkmnpqrstuvwxyz23456789";
  return [...crypto.getRandomValues(new Uint8Array(10))].map((b) => abc[b % abc.length]).join("");
}
function sixDigits() {
  const n = crypto.getRandomValues(new Uint32Array(1))[0] % 1000000;
  return String(n).padStart(6, "0");
}
function cleanEmail(e) {
  e = String(e || "").trim().toLowerCase();
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(e) && e.length <= 200 ? e : "";
}
async function body(req) {
  try { return await req.json(); } catch { return {}; }
}

/// Fixed-window counter in D1: true while `key` was hit at most `max` times in the last `window` seconds.
async function allow(env, key, max, window) {
  const t = now();
  const r = await env.DB.prepare(
    "INSERT INTO limits (k, n, reset) VALUES (?, 1, ?) ON CONFLICT(k) DO UPDATE SET "
    + "n = CASE WHEN reset <= ? THEN 1 ELSE n + 1 END, reset = CASE WHEN reset <= ? THEN ? ELSE reset END RETURNING n"
  ).bind(key, t + window, t, t, t + window).first();
  return !r || r.n <= max;
}
const ipOf = (req) => req.headers.get("cf-connecting-ip") || "?";

async function sendCode(env, email, code) {
  if (!env.RESEND_KEY) {
    console.log(`sign-in code for ${email}: ${code}`); // dev: read it with "npx wrangler tail"
    return true;
  }
  const r = await fetch("https://api.resend.com/emails", {
    method: "POST",
    headers: { authorization: `Bearer ${env.RESEND_KEY}`, "content-type": "application/json" },
    body: JSON.stringify({
      from: env.MAIL_FROM,
      to: [email],
      subject: `${code} is your Echo sign-in code`,
      text: `Your Echo sign-in code is ${code}\n\nIt works for ${CODE_MINUTES} minutes. If you didn't ask for it, ignore this email.`,
      html: `<div style="font-family:system-ui,Segoe UI,Arial,sans-serif;max-width:420px;margin:0 auto;padding:24px;color:#111">`
        + `<h2 style="margin:0 0 16px">Your Echo sign-in code</h2>`
        + `<div style="font-size:32px;font-weight:700;letter-spacing:6px;background:#f2f2f5;border-radius:10px;padding:16px;text-align:center">${code}</div>`
        + `<p style="color:#555;font-size:14px;margin-top:16px">It works for ${CODE_MINUTES} minutes. If you didn't ask for it, you can ignore this email.</p></div>`,
    }),
  });
  if (!r.ok) console.log("resend failed", r.status, await r.text());
  return r.ok;
}

async function userFromToken(req, env) {
  const h = req.headers.get("authorization") || "";
  const token = h.startsWith("Bearer ") ? h.slice(7).trim() : "";
  if (!token) return null;
  const th = await sha256(token);
  const s = await env.DB.prepare("SELECT user_id, last_seen FROM sessions WHERE token_hash = ?").bind(th).first();
  if (!s) return null;
  if (s.last_seen < now() - SESSION_DAYS * 86400) {
    await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ?").bind(th).run();
    return null;
  }
  await env.DB.prepare("UPDATE sessions SET last_seen = ? WHERE token_hash = ?").bind(now(), th).run();
  return env.DB.prepare("SELECT * FROM users WHERE id = ?").bind(s.user_id).first();
}

/// bytes stored under u/<id>/ (counted from R2, cached for a few minutes)
async function usage(env, user, fresh = false) {
  if (now() - user.used_at < (fresh ? USAGE_FRESH_SECONDS : USAGE_CACHE_SECONDS)) return user.used_bytes;
  let used = 0, cursor;
  for (let page = 0; page < 200; page++) {
    const l = await env.BUCKET.list({ prefix: `u/${user.id}/`, cursor, limit: 1000 });
    for (const o of l.objects) used += o.size;
    if (!l.truncated) break;
    cursor = l.cursor;
  }
  await env.DB.prepare("UPDATE users SET used_bytes = ?, used_at = ? WHERE id = ?").bind(used, now(), user.id).run();
  return used;
}

function me(user, used) {
  // over_since > 0: over the plan, clips above it go on delete_at (Echo shows a warning)
  const since = user.over_since || 0;
  return { id: user.id, email: user.email, plan: user.plan, paid: !!user.polar_sub, until: user.plan_until || 0, used, quota: user.quota_bytes,
    over_since: since, delete_at: since ? since + OVER_GRACE_DAYS * 86400 : 0 };
}

async function authStart(req, env) {
  const email = cleanEmail((await body(req)).email);
  if (!email) return fail("Enter a valid email address");
  // stop guessing (new code = 5 new tries) and email spam / using up the Resend quota
  if (!(await allow(env, `ip:${ipOf(req)}`, 10, 3600)) || !(await allow(env, `mail:${email}`, 5, 3600)))
    return fail("Too many sign-in emails - try again in an hour", 429);
  if (!(await allow(env, "mail:all", Number(env.MAIL_DAY_LIMIT || 90), 86400)))
    return fail("Echo Cloud can't send more sign-in emails today - try again tomorrow", 429);
  const old = await env.DB.prepare("SELECT sent FROM codes WHERE email = ?").bind(email).first();
  if (old && now() - old.sent < 45) return fail("A code was just sent - wait a moment before asking again", 429);
  const code = sixDigits();
  await env.DB.prepare(
    "INSERT OR REPLACE INTO codes (email, code_hash, expires, tries, sent) VALUES (?, ?, ?, 0, ?)"
  ).bind(email, await sha256(`${email}:${code}`), now() + CODE_MINUTES * 60, now()).run();
  if (!(await sendCode(env, email, code))) return fail("Couldn't send the email - try again later", 502);
  return json({ ok: true });
}

async function authVerify(req, env) {
  const b = await body(req);
  const email = cleanEmail(b.email);
  const code = String(b.code || "").replace(/\D/g, "");
  if (!email || code.length !== 6) return fail("Enter the 6-digit code from the email");
  if (!(await allow(env, `try:${email}`, 10, 3600)) || !(await allow(env, `tryip:${ipOf(req)}`, 30, 3600)))
    return fail("Too many tries - wait an hour and ask for a new code", 429);
  const row = await env.DB.prepare("SELECT * FROM codes WHERE email = ?").bind(email).first();
  if (!row || row.expires < now()) return fail("The code expired - ask for a new one");
  if (row.tries >= 5) return fail("Too many wrong codes - ask for a new one");
  if ((await sha256(`${email}:${code}`)) !== row.code_hash) {
    await env.DB.prepare("UPDATE codes SET tries = tries + 1 WHERE email = ?").bind(email).run();
    return fail("Wrong code");
  }
  await env.DB.prepare("DELETE FROM codes WHERE email = ?").bind(email).run();
  let user = await env.DB.prepare("SELECT * FROM users WHERE email = ?").bind(email).first();
  if (!user) {
    // one person making lots of free accounts: at most 3 new accounts per network per day
    if (!(await allow(env, `newacc:${ipOf(req)}`, Number(env.NEW_ACCOUNTS_PER_DAY || 3), 86400)))
      return fail("Too many new Echo Cloud accounts from this network today - try again tomorrow", 429);
    const id = randomId();
    const quota = Number(env.FREE_GB || 10) * GB;
    await env.DB.prepare(
      "INSERT INTO users (id, email, plan, quota_bytes, used_bytes, used_at, created) VALUES (?, ?, 'free', ?, 0, 0, ?)"
    ).bind(id, email, quota, now()).run();
    user = await env.DB.prepare("SELECT * FROM users WHERE id = ?").bind(id).first();
  }
  const token = randomHex(32);
  await env.DB.prepare("INSERT INTO sessions (token_hash, user_id, created, last_seen) VALUES (?, ?, ?, ?)")
    .bind(await sha256(token), user.id, now(), now()).run();
  return json({ token, ...me(user, await usage(env, user)) });
}

async function creds(user, env) {
  if (!env.CF_API_TOKEN || !env.ACCOUNT_ID || !env.PARENT_ACCESS_KEY_ID)
    return fail("Echo Cloud storage isn't set up yet", 503);
  const used = await usage(env, user, true);
  const full = used >= user.quota_bytes;
  const readOnly = full || env.UPLOAD_KEYS === "ro";
  const r = await fetch(`https://api.cloudflare.com/client/v4/accounts/${env.ACCOUNT_ID}/r2/temp-access-credentials`, {
    method: "POST",
    headers: { authorization: `Bearer ${env.CF_API_TOKEN}`, "content-type": "application/json" },
    body: JSON.stringify({
      bucket: env.BUCKET_NAME,
      parentAccessKeyId: env.PARENT_ACCESS_KEY_ID,
      permission: readOnly ? "object-read-only" : "object-read-write",
      ttlSeconds: CREDS_SECONDS,
      prefixes: [`u/${user.id}/`],
    }),
  });
  const j = await r.json().catch(() => ({}));
  if (!r.ok || !j.success) {
    console.log("temp creds failed", r.status, JSON.stringify(j.errors || j));
    return fail("Echo Cloud couldn't hand out storage keys - try again later", 502);
  }
  return json({
    endpoint: `https://${env.ACCOUNT_ID}.r2.cloudflarestorage.com`,
    bucket: env.BUCKET_NAME,
    root: `${env.BUCKET_NAME}/u/${user.id}`,
    access_key_id: j.result.accessKeyId,
    secret_access_key: j.result.secretAccessKey,
    session_token: j.result.sessionToken,
    expires: now() + CREDS_SECONDS,
    read_only: full,
    keys_read_only: readOnly,
    writes: "server",   // this server has /upload, /delete, /move (Echo from chat 15 uses them)
    ...me(user, used),
  });
}

// ----- writes through the server: one presigned upload per file (size checked against the plan), delete, move -----
const enc3986 = (s) => encodeURIComponent(s).replace(/[!'()*]/g, (c) => "%" + c.charCodeAt(0).toString(16).toUpperCase());
async function hmac(key, msg) {
  const k = await crypto.subtle.importKey("raw", typeof key === "string" ? new TextEncoder().encode(key) : key, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await crypto.subtle.sign("HMAC", k, new TextEncoder().encode(msg)));
}
const hex = (b) => [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
/// AWS Signature V4 presigned URL (query auth). `headers` = extra headers that must be sent exactly like this
/// (e.g. content-length, so the upload can't be bigger than what was checked). Exported for the test.
export async function presign({ method, host, path, accessKey, secretKey, sessionToken, region = "auto", service = "s3", expires = 3600, headers = {}, date = new Date() }) {
  const amzDate = date.toISOString().replace(/[-:]/g, "").replace(/\.\d{3}/, "");
  const day = amzDate.slice(0, 8);
  const scope = `${day}/${region}/${service}/aws4_request`;
  const hs = { host, ...Object.fromEntries(Object.entries(headers).map(([k, v]) => [k.toLowerCase(), String(v).trim()])) };
  const signed = Object.keys(hs).sort();
  const q = {
    "X-Amz-Algorithm": "AWS4-HMAC-SHA256",
    "X-Amz-Credential": `${accessKey}/${scope}`,
    "X-Amz-Date": amzDate,
    "X-Amz-Expires": String(expires),
    "X-Amz-SignedHeaders": signed.join(";"),
  };
  if (sessionToken) q["X-Amz-Security-Token"] = sessionToken;
  const query = Object.keys(q).sort().map((k) => `${enc3986(k)}=${enc3986(q[k])}`).join("&");
  const canonPath = path.split("/").map(enc3986).join("/");
  const canon = [method, canonPath, query, signed.map((k) => `${k}:${hs[k]}\n`).join(""), signed.join(";"), "UNSIGNED-PAYLOAD"].join("\n");
  const toSign = ["AWS4-HMAC-SHA256", amzDate, scope, await sha256(canon)].join("\n");
  let k = await hmac(`AWS4${secretKey}`, day);
  for (const part of [region, service, "aws4_request"]) k = await hmac(k, part);
  const sig = hex(await hmac(k, toSign));
  return `https://${host}${canonPath}?${query}&X-Amz-Signature=${sig}`;
}

/// "Echo/2026/11/02/FiveM/clip.mp4" -> "u/<id>/Echo/…" (null if the path is odd)
function userKey(user, p) {
  const path = String(p || "").replace(/^\/+/, "");
  if (!path || path.length > 1000 || path.split("/").some((x) => x === ".." || x === "" || x === ".")) return null;
  return `u/${user.id}/${path}`;
}

/// temporary R2 keys for exactly one object (or a prefix)
async function tempKeys(env, permission, prefixes, ttl) {
  const r = await fetch(`https://api.cloudflare.com/client/v4/accounts/${env.ACCOUNT_ID}/r2/temp-access-credentials`, {
    method: "POST",
    headers: { authorization: `Bearer ${env.CF_API_TOKEN}`, "content-type": "application/json" },
    body: JSON.stringify({ bucket: env.BUCKET_NAME, parentAccessKeyId: env.PARENT_ACCESS_KEY_ID, permission, ttlSeconds: ttl, prefixes }),
  });
  const j = await r.json().catch(() => ({}));
  if (!r.ok || !j.success) { console.log("temp creds failed", r.status, JSON.stringify(j.errors || j)); return null; }
  return j.result;
}

const UPLOAD_SECONDS = 6 * 3600;   // big clips on slow connections
async function upload(req, user, env) {
  if (!env.CF_API_TOKEN || !env.ACCOUNT_ID || !env.PARENT_ACCESS_KEY_ID) return fail("Echo Cloud storage isn't set up yet", 503);
  const b = await body(req);
  const key = userKey(user, b.path);
  const size = Math.floor(Number(b.size));
  if (!key) return fail("Bad clip path");
  if (!(size > 0) || size > 5e9) return fail("Clips over 5 GB can't be uploaded to Echo Cloud yet");
  if (!(await allow(env, `up:${user.id}`, 600, 3600))) return fail("Too many uploads - try again in a while", 429);
  // uploads that got a link but aren't finished yet still count (else many links could be fetched first and
  // all used later); finished ones are in R2 and counted by usage(), expired ones are forgotten
  const pend = (await env.DB.prepare("SELECT key, size FROM uploads WHERE user_id = ? AND expires > ?").bind(user.id, now()).all()).results || [];
  if (pend.length > 50) return fail("Too many uploads at once - wait for some to finish", 429);
  let pending = 0;
  for (const p of pend) {
    if (p.key === key) continue;   // this file again (a retry): replaced below
    const o = await env.BUCKET.head(p.key);
    if (o && o.size === p.size) await env.DB.prepare("DELETE FROM uploads WHERE key = ?").bind(p.key).run();
    else pending += p.size;
  }
  await env.DB.prepare("DELETE FROM uploads WHERE expires <= ?").bind(now()).run();
  const used = await usage(env, user, true);
  const old = await env.BUCKET.head(key);
  const after = used + pending - (old ? old.size : 0) + size;
  if (after > user.quota_bytes)
    return json({ error: `Echo Cloud is full - this clip needs ${(size / GB).toFixed(2)} GB but only ${(Math.max(0, user.quota_bytes - used - pending) / GB).toFixed(2)} GB is left`, full: true, used: used + pending, quota: user.quota_bytes }, 413);
  const k = await tempKeys(env, "object-read-write", [key], UPLOAD_SECONDS);
  if (!k) return fail("Echo Cloud couldn't prepare the upload - try again later", 502);
  const url = await presign({
    method: "PUT", host: `${env.ACCOUNT_ID}.r2.cloudflarestorage.com`, path: `/${env.BUCKET_NAME}/${key}`,
    accessKey: k.accessKeyId, secretKey: k.secretAccessKey, sessionToken: k.sessionToken,
    expires: UPLOAD_SECONDS, headers: { "content-length": size },
  });
  await env.DB.prepare("INSERT OR REPLACE INTO uploads (key, user_id, size, expires) VALUES (?, ?, ?, ?)")
    .bind(key, user.id, size, now() + UPLOAD_SECONDS).run();
  return json({ url, size, expires: now() + UPLOAD_SECONDS });
}

async function deleteKeys(env, user, keys) {
  if (!keys.length) return;
  for (let i = 0; i < keys.length; i += 1000) await env.BUCKET.delete(keys.slice(i, i + 1000));
  // their share links go too
  for (const key of keys) {
    const rows = (await env.DB.prepare("SELECT id FROM shares WHERE key = ? AND user_id = ?").bind(key, user.id).all()).results || [];
    for (const r of rows) await env.BUCKET.delete(`thumbs/${r.id}.jpg`);
    if (rows.length) await env.DB.prepare("DELETE FROM shares WHERE key = ? AND user_id = ?").bind(key, user.id).run();
  }
  await env.DB.prepare("UPDATE users SET used_at = 0 WHERE id = ?").bind(user.id).run();   // recount next time
}

async function del(req, user, env) {
  const b = await body(req);
  const keys = (Array.isArray(b.paths) ? b.paths : [b.path]).slice(0, 1000).map((p) => userKey(user, p));
  if (!keys.length || keys.some((k) => !k)) return fail("Bad clip path");
  await deleteKeys(env, user, keys);
  return json({ ok: true, deleted: keys.length });
}

async function move(req, user, env) {
  const b = await body(req);
  const from = userKey(user, b.from), to = userKey(user, b.to);
  if (!from || !to) return fail("Bad clip path");
  if (from === to) return json({ ok: true });
  const o = await env.BUCKET.get(from);
  if (!o) return fail("That clip isn't in Echo Cloud", 404);
  // R2 has no server-side copy in Workers: stream it over (doesn't use more space for long: the old one is deleted)
  const { readable, writable } = new FixedLengthStream(o.size);
  const put = env.BUCKET.put(to, readable, { httpMetadata: o.httpMetadata, customMetadata: o.customMetadata });
  await o.body.pipeTo(writable);
  await put;
  await env.BUCKET.delete(from);
  await env.DB.prepare("UPDATE shares SET key = ? WHERE key = ? AND user_id = ?").bind(to, from, user.id).run();
  return json({ ok: true });
}

// share copies (watermark / AV1 -> H.264, or clips that live on another cloud) only exist for their link
const isShareCopy = (key) => key.includes("/Echo/.shared/");

/// "Stop sharing": the link stops working, the clip stays (a share copy made just for the link is deleted)
async function unshare(req, user, env) {
  const key = userKey(user, (await body(req)).path);
  if (!key) return fail("Bad clip path");
  const rows = (await env.DB.prepare("SELECT id FROM shares WHERE key = ? AND user_id = ?").bind(key, user.id).all()).results || [];
  for (const r of rows) await env.BUCKET.delete(`thumbs/${r.id}.jpg`);
  await env.DB.prepare("DELETE FROM shares WHERE key = ? AND user_id = ?").bind(key, user.id).run();
  let copyDeleted = false;
  if (isShareCopy(key) && (await env.BUCKET.head(key))) {
    await env.BUCKET.delete(key);
    await env.DB.prepare("UPDATE users SET used_at = 0 WHERE id = ?").bind(user.id).run();   // recount next time
    copyDeleted = true;
  }
  return json({ ok: true, removed: rows.length, copy_deleted: copyDeleted });
}

/// Echo → Settings → About → "Send feedback": one email to support (Resend), the log as an attachment.
/// No account needed; limited per network and per day so nobody can flood the inbox or the Resend quota.
async function feedback(req, env, user) {
  const b = await body(req);
  const message = String(b.message || "").replace(/\r/g, "").trim().slice(0, 4000);
  if (!message) return fail("Write a few words first");
  if (!(await allow(env, `fb:ip:${ipOf(req)}`, 5, 3600))) return fail("That's a lot of feedback at once - try again in an hour", 429);
  if (!(await allow(env, "fb:day", Number(env.FEEDBACK_DAY_LIMIT || 40), 86400))) return fail("Feedback is paused for today - email support@echoclip.link", 429);
  const contact = cleanEmail(b.contact);
  const account = user && user.email ? user.email : "";
  const clip = (t, n) => String(t || "").replace(/[\u0000-\u001f]/g, " ").slice(0, n);
  const lines = [
    message, "", "---",
    `From: ${contact || "(no email given)"}`,
    `Echo Cloud account: ${account || "(not signed in)"}${user && user.plan ? " · " + user.plan : ""}`,
    `Echo ${clip(b.version, 40) || "?"} on ${clip(b.os, 20) || "?"}`,
    `Country: ${req.cf && req.cf.country ? req.cf.country : "?"}`,
  ];
  const log = typeof b.log === "string" ? b.log.slice(-200000) : "";
  if (!env.RESEND_KEY) { console.log("feedback:", lines.join("\n"), log ? `(+ log ${log.length} chars)` : ""); return json({ ok: true }); }
  const mail = {
    from: env.MAIL_FROM,
    to: [env.SUPPORT_TO || "support@echoclip.link"],
    subject: `Echo feedback: ${clip(message.split("\n")[0], 70)}`,
    text: lines.join("\n"),
  };
  const reply = contact || account;
  if (reply) mail.reply_to = reply;
  if (log) {
    const bytes = new TextEncoder().encode(log);
    let bin = ""; for (let i = 0; i < bytes.length; i += 8192) bin += String.fromCharCode(...bytes.subarray(i, i + 8192));
    mail.attachments = [{ filename: "echo-log.txt", content: btoa(bin) }];
  }
  const r = await fetch("https://api.resend.com/emails", {
    method: "POST",
    headers: { authorization: `Bearer ${env.RESEND_KEY}`, "content-type": "application/json" },
    body: JSON.stringify(mail),
  });
  if (!r.ok) { console.log("feedback mail failed", r.status, await r.text()); return fail("Couldn't send it right now - try again later", 502); }
  return json({ ok: true });
}

// ----- support logs: Echo → Settings → "Send log" uploads the log, the person pastes the link -----
// Echo strips passwords, keys and email addresses before uploading. Readable by anyone with the link (14 random
// characters, ~70 bits - can't be guessed), deleted after LOG_DAYS. Meant for William + Claude to read without
// passing files around (chat 18, Will's request).
const LOG_DAYS = 14;
const LOG_MAX = 400_000;
function logId() {
  const abc = "abcdefghijkmnpqrstuvwxyz23456789";
  return [...crypto.getRandomValues(new Uint8Array(14))].map((b) => abc[b % abc.length]).join("");
}
async function logUpload(req, env, origin) {
  if (!(await allow(env, `log:${ipOf(req)}`, 10, 3600))) return fail("Too many logs from this network - try again in an hour", 429);
  if (!(await allow(env, "log:day", Number(env.LOG_DAY_LIMIT || 500), 86400))) return fail("Log uploads are paused for today", 429);
  const b = await body(req);
  const text = String(b.text || "");
  if (!text.trim()) return fail("Empty log");
  if (text.length > LOG_MAX) return fail("Log too big", 413);
  const id = logId();
  await env.BUCKET.put(`logs/${id}.txt`, text, {
    httpMetadata: { contentType: "text/plain; charset=utf-8" },
    customMetadata: { v: String(b.version || "").slice(0, 20), os: String(b.os || "").slice(0, 20) },
  });
  console.log("log uploaded", id, text.length, b.version || "");
  return json({ id, url: `${origin}/log/${id}` });
}
async function logRead(req, env, id) {
  if (!(await allow(env, `logread:${ipOf(req)}`, 120, 3600))) return new Response("slow down", { status: 429 });
  const o = await env.BUCKET.get(`logs/${id}.txt`);
  if (!o || now() - Math.floor(o.uploaded.getTime() / 1000) > LOG_DAYS * 86400) return new Response("This log doesn't exist or has expired (logs are kept 14 days).", { status: 404, headers: { "content-type": "text/plain; charset=utf-8" } });
  return new Response(o.body, { headers: {
    "content-type": "text/plain; charset=utf-8", "cache-control": "private, no-store", "x-robots-tag": "noindex, nofollow",
    "x-content-type-options": "nosniff", "content-security-policy": "sandbox",
  } });
}
/// Daily (cron): logs older than LOG_DAYS go
async function cleanupLogs(env) {
  const l = await env.BUCKET.list({ prefix: "logs/", limit: 1000 });
  const old = l.objects.filter((o) => now() - Math.floor(new Date(o.uploaded).getTime() / 1000) > LOG_DAYS * 86400).map((o) => o.key);
  if (old.length) await env.BUCKET.delete(old);
  console.log("logs: deleted", old.length);
}

/// Daily (cron): a copy of the database (accounts, sessions, share links) in R2 under backups/, the last 14 kept.
/// Not reachable from outside (share links only serve objects listed in the shares table; user keys only see u/<id>/).
/// Restore: download the file in the Cloudflare dashboard (R2 → echo-cloud → backups/), the tables are JSON rows.
async function backupDb(env) {
  await migrate(env);
  const tables = (await env.DB.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'").all()).results || [];
  const dump = { made: new Date().toISOString(), tables: {} };
  for (const { name } of tables) {
    if (name === "limits" || name === "codes") continue;   // short-lived, nothing to restore
    dump.tables[name] = (await env.DB.prepare(`SELECT * FROM "${name.replace(/"/g, "")}"`).all()).results || [];
  }
  const day = dump.made.slice(0, 10);
  const gz = new Response(new Blob([JSON.stringify(dump)]).stream().pipeThrough(new CompressionStream("gzip")));
  await env.BUCKET.put(`backups/db-${day}.json.gz`, await gz.arrayBuffer(), { httpMetadata: { contentType: "application/gzip" } });
  const list = await env.BUCKET.list({ prefix: "backups/db-" });
  const old = list.objects.map((o) => o.key).sort().slice(0, -14);
  if (old.length) await env.BUCKET.delete(old);
  console.log(`backup: ${day}, ${Object.keys(dump.tables).length} tables`);
}

/// Daily (cron in wrangler.toml): links that have expired lose their share copy, so the space comes back.
/// Links to the clip itself just stop working (the clip stays).
async function cleanupExpired(env) {
  await migrate(env);
  const rows = (await env.DB.prepare("SELECT id, key, user_id FROM shares WHERE expires > 0 AND expires < ? LIMIT 500").bind(now()).all()).results || [];
  let copies = 0;
  for (const r of rows) {
    if (!isShareCopy(r.key)) continue;
    await env.BUCKET.delete(`thumbs/${r.id}.jpg`);
    await env.DB.prepare("DELETE FROM shares WHERE id = ?").bind(r.id).run();
    const still = await env.DB.prepare("SELECT 1 FROM shares WHERE key = ? LIMIT 1").bind(r.key).first();
    if (!still) {
      await env.BUCKET.delete(r.key);
      await env.DB.prepare("UPDATE users SET used_at = 0 WHERE id = ?").bind(r.user_id).run();
      copies++;
    }
  }
  console.log("cleanup: expired links", rows.length, "share copies deleted", copies);
  return { expired: rows.length, copies };
}

// Share copies (Echo/.shared/…) that no link uses any more - left behind by app bugs (chat 27: links forgotten
// after a delete / move) or crashes between upload and /share. Daily, a day's grace (a link being made right now
// uploads its copy first), at most 2000 deletions per run. Normal clips are never touched.
async function cleanupStrayCopies(env) {
  await migrate(env);
  const users = (await env.DB.prepare("SELECT id FROM users").all()).results || [];
  const old = Date.now() - 86400 * 1000;
  let checked = 0, deleted = 0, freed = 0;
  for (const u of users) {
    let cursor;
    let touched = false;
    do {
      const l = await env.BUCKET.list({ prefix: `u/${u.id}/Echo/.shared/`, cursor, limit: 1000 });
      for (const o of l.objects) {
        checked++;
        if (o.uploaded && o.uploaded.getTime() > old) continue;
        const used = await env.DB.prepare("SELECT 1 FROM shares WHERE key = ? LIMIT 1").bind(o.key).first();
        if (used) continue;
        await env.BUCKET.delete(o.key);
        deleted++; freed += o.size || 0; touched = true;
        if (deleted >= 2000) break;
      }
      cursor = l.truncated && deleted < 2000 ? l.cursor : undefined;
    } while (cursor);
    if (touched) await env.DB.prepare("UPDATE users SET used_at = 0 WHERE id = ?").bind(u.id).run();   // recount next time
    if (deleted >= 2000) break;
  }
  console.log("cleanup: stray share copies", { checked, deleted, freed });
  return { checked, deleted, freed };
}

// ----- over the plan (terms: email at least 30 days before clips above the plan are removed) -----
// A paid plan that ended leaves its clips in R2 (William pays for them). Daily: warn by email, remind after
// 23 days, after 30 days remove the oldest clips until the account fits its plan again (clips with a working
// share link last). Never touches accounts with a paid plan; nothing is removed unless the warning email went
// out. Freeing up space or paying again cancels it (next run / webhook).
const fmtGB = (b) => `${+(b / GB).toFixed(b < 100 * GB ? 1 : 0)} GB`;
const fmtDay = (t) => new Date(t * 1000).toLocaleDateString("en-GB", { day: "numeric", month: "long", year: "numeric", timeZone: "UTC" });

async function plainMail(env, to, subject, text) {
  if (!env.RESEND_KEY) { console.log("mail (no RESEND_KEY):", to, subject); return false; }
  const r = await fetch("https://api.resend.com/emails", {
    method: "POST",
    headers: { authorization: `Bearer ${env.RESEND_KEY}`, "content-type": "application/json" },
    body: JSON.stringify({ from: env.MAIL_FROM, to: [to], reply_to: env.SUPPORT_TO || "support@echoclip.link", subject, text }),
  });
  if (!r.ok) console.log("mail failed", r.status, await r.text());
  return r.ok;
}

async function enforceQuota(env) {
  await migrate(env);
  const day = 86400, t = now();
  // used_bytes is the last count; it's recounted below for every candidate
  const rows = (await env.DB.prepare("SELECT * FROM users WHERE (used_bytes > quota_bytes OR over_since > 0) ORDER BY over_since DESC LIMIT 25").all()).results || [];
  const report = [];
  for (const u of rows) {
    const set = (sql, ...v) => env.DB.prepare(`UPDATE users SET ${sql} WHERE id = ?`).bind(...v, u.id).run();
    if (u.polar_sub) {   // paid plan running: never
      if (u.over_since) await set("over_since = 0, over_stage = 0, over_removed = 0");
      continue;
    }
    const used = await usage(env, { ...u, used_at: 0 });
    const quota = u.quota_bytes, limit = fmtGB(quota);
    if (used <= quota) {
      if (u.over_since) { await set("over_since = 0, over_stage = 0, over_removed = 0"); report.push([u.id, "back under"]); console.log("quota:", u.id, "back under the plan"); }
      continue;
    }
    if (!u.over_since) {
      const at = t + 30 * day;
      const ok = await plainMail(env, u.email, `Your Echo Cloud holds more than your ${limit} plan`,
        `Hi,\n\nYour Echo Cloud holds ${fmtGB(used)} of clips, more than your plan's ${limit} (this happens when a paid plan ends).\n\n`
        + `Nothing has been deleted. You can still watch, download, share and delete your clips - you just can't upload new ones.\n\n`
        + `On ${fmtDay(at)} we'll remove your oldest clips from Echo Cloud until you're under ${limit} (clips with a working share link go last), unless before then you:\n`
        + `- delete clips you don't need, or download the ones you want to keep (in Echo: Cloud), or\n`
        + `- pick a plan again (Echo: Settings > Cloud > Upgrade).\n\n`
        + `Clips on your PC are never touched. Questions? Just reply to this email.\n\nEcho - echoclip.link`);
      if (ok) { await set("over_since = ?, over_stage = 1, over_removed = 0", t); report.push([u.id, "warned"]); console.log("quota:", u.id, "warned", fmtGB(used), ">", limit); }
      continue;
    }
    const due = u.over_since + OVER_GRACE_DAYS * day;
    if (t < due) {
      if (u.over_stage < 2 && t >= u.over_since + OVER_REMIND_DAYS * day) {
        const ok = await plainMail(env, u.email, `Reminder: Echo Cloud removes clips on ${fmtDay(due)}`,
          `Hi,\n\nA reminder: your Echo Cloud holds ${fmtGB(used)}, more than your plan's ${limit}.\n\n`
          + `On ${fmtDay(due)} we'll remove your oldest clips until you're under ${limit}. To keep them, delete or download clips in Echo (Cloud), or pick a plan again (Settings > Cloud > Upgrade).\n\n`
          + `Clips on your PC are never touched.\n\nEcho - echoclip.link`);
        if (ok) { await set("over_stage = 2"); report.push([u.id, "reminded"]); }
      }
      continue;
    }
    if (u.over_stage < 1) continue;   // never remove without the warning email
    // remove: oldest first, clips with a working share link last
    const objs = [];
    let cursor;
    for (let page = 0; page < 200; page++) {
      const l = await env.BUCKET.list({ prefix: `u/${u.id}/`, cursor, limit: 1000 });
      objs.push(...l.objects);
      if (!l.truncated) break;
      cursor = l.cursor;
    }
    const shared = new Set((((await env.DB.prepare("SELECT key FROM shares WHERE user_id = ? AND (expires = 0 OR expires > ?)").bind(u.id, t).all()).results) || []).map((r) => r.key));
    const ts = (o) => (o.uploaded ? new Date(o.uploaded).getTime() : 0);
    objs.sort((a, b) => (shared.has(a.key) - shared.has(b.key)) || (ts(a) - ts(b)));
    const keys = [];
    let left = objs.reduce((n, o) => n + o.size, 0), freed = 0;
    for (const o of objs) {
      if (left <= quota || keys.length >= OVER_DELETE_PER_RUN) break;
      keys.push(o.key); left -= o.size; freed += o.size;
    }
    if (keys.length) {
      await env.BUCKET.delete(keys);
      for (let i = 0; i < keys.length; i += 50) {
        const part = keys.slice(i, i + 50), q = part.map(() => "?").join(",");
        const ids = (((await env.DB.prepare(`SELECT id FROM shares WHERE user_id = ? AND key IN (${q})`).bind(u.id, ...part).all()).results) || []).map((r) => r.id);
        if (ids.length) {
          await env.BUCKET.delete(ids.map((id) => `thumbs/${id}.jpg`));
          await env.DB.prepare(`DELETE FROM shares WHERE user_id = ? AND key IN (${q})`).bind(u.id, ...part).run();
        }
      }
    }
    const removed = (u.over_removed || 0) + keys.length;
    await set("used_at = 0, over_stage = 3, over_removed = ?", removed);
    console.log("quota:", u.id, "removed", keys.length, "files,", fmtGB(freed), left <= quota ? "- now under the plan" : "- more next run");
    report.push([u.id, "removed", keys.length]);
    if (left <= quota) {
      await plainMail(env, u.email, "Echo Cloud: clips above your plan were removed",
        `Hi,\n\nAs announced, we removed ${removed} of your oldest files from Echo Cloud so it fits your plan's ${limit} again.\n\n`
        + `Clips on your PC were not touched. Want more space again? Echo: Settings > Cloud > Upgrade.\n\nEcho - echoclip.link`);
      await set("over_since = 0, over_stage = 0, over_removed = 0");
    }
  }
  return report;
}

/// Delete the account and everything in it. Big accounts take several calls: {done:false} = call again.
async function deleteAccount(req, user, env) {
  const b = await body(req);
  if (cleanEmail(b.confirm) !== user.email) return fail("Type your email address to confirm");
  if (user.polar_sub && (user.plan_until || 0) > now())
    return fail("You still have a paid plan. Cancel it under Manage plan first - when the paid time ends you can delete the account, or email support@echoclip.link to delete it right away.", 409);
  let n = 0;
  for (let round = 0; round < 5; round++) {
    const l = await env.BUCKET.list({ prefix: `u/${user.id}/`, limit: 1000 });
    if (!l.objects.length) break;
    await env.BUCKET.delete(l.objects.map((o) => o.key));
    n += l.objects.length;
    if (!l.truncated) break;
  }
  const left = await env.BUCKET.list({ prefix: `u/${user.id}/`, limit: 1 });
  if (left.objects.length) return json({ done: false, deleted: n });
  const shares = (await env.DB.prepare("SELECT id FROM shares WHERE user_id = ?").bind(user.id).all()).results || [];
  for (let i = 0; i < shares.length; i += 1000) await env.BUCKET.delete(shares.slice(i, i + 1000).map((r) => `thumbs/${r.id}.jpg`));
  await env.DB.batch([
    env.DB.prepare("DELETE FROM shares WHERE user_id = ?").bind(user.id),
    env.DB.prepare("DELETE FROM sessions WHERE user_id = ?").bind(user.id),
    env.DB.prepare("DELETE FROM codes WHERE email = ?").bind(user.email),
    env.DB.prepare("DELETE FROM uploads WHERE user_id = ?").bind(user.id),
    env.DB.prepare("DELETE FROM users WHERE id = ?").bind(user.id),
    env.DB.prepare("DELETE FROM pins WHERE user_id = ?").bind(user.id),
    env.DB.prepare("DELETE FROM follows WHERE follower = ? OR followee = ?").bind(user.id, user.id),
    env.DB.prepare("DELETE FROM profiles WHERE user_id = ?").bind(user.id),
  ]);
  console.log("account deleted", user.id);
  return json({ done: true, deleted: n });
}

// ----- website counts for the stats page: no cookies, nothing personal kept -----
// The pages are static files, so site.js pings POST /hit {t:"view",p,r} or {t:"click",k}; downloads are counted
// in /download. Unique visitors: a scrambled code of day + IP + browser (kept 2 days in uv, only to not count anyone twice).
const BOT_UA = /bot|crawl|spider|slurp|preview|facebookexternalhit|embed|curl|wget|python|go-http|headless|lighthouse|monitor|scan/i;
const SITE_PAGES = new Set(["/", "/download", "/pricing", "/changelog", "/privacy", "/terms", "/code-signing"]);
const SITE_CLICKS = new Set(["plans", "storage", "features", "discord", "changelog", "github", "download"]);
const utcDay = (t = now()) => new Date(t * 1000).toISOString().slice(0, 10);
const isBot = (req) => { const ua = req.headers.get("user-agent") || ""; return !ua || BOT_UA.test(ua); };
async function bump(env, k, n = 1) {
  await env.DB.prepare("INSERT INTO stats (day, k, n) VALUES (?, ?, ?) ON CONFLICT(day, k) DO UPDATE SET n = n + excluded.n")
    .bind(utcDay(), k, n).run();
}
async function hit(req, env) {
  const done = new Response(null, { status: 204 });
  if (isBot(req) || !(await allow(env, `hit:${ipOf(req)}`, 300, 3600))) return done;
  const b = await body(req);
  if (b.t === "view") {
    let p = String(b.p || "/").split(/[?#]/)[0].replace(/\.html$/, "").replace(/\/+$/, "") || "/";
    if (p === "/index") p = "/";
    await bump(env, "view:" + (SITE_PAGES.has(p) ? p : "other"));
    const h = (await sha256(`${utcDay()}|${ipOf(req)}|${req.headers.get("user-agent")}|${env.ADMIN_KEY || "echo"}`)).slice(0, 24);
    const r = await env.DB.prepare("INSERT OR IGNORE INTO uv (day, h) VALUES (?, ?)").bind(utcDay(), h).run();
    if (((r && r.meta) ? r.meta.changes : r && r.changes) > 0) await bump(env, "visitors");
    let ref = "";
    try { ref = new URL(String(b.r || "")).hostname.replace(/^www\./, "").slice(0, 60); } catch { /* none */ }
    if (ref && !/(^|\.)echoclip\.link$/.test(ref)) await bump(env, "ref:" + ref);
  } else if (b.t === "click" && SITE_CLICKS.has(b.k)) await bump(env, "click:" + b.k);
  return done;
}
// Only real clicks count, once per person per file per day (chat 20: the same 5 hits on all four files in a day =
// link scanners / antivirus / prefetch, not people). Browsers mark a click on a link with Sec-Fetch-Mode "navigate";
// scanners and prefetchers don't (or send "no-cors"/"prefetch"). Anything without the header (old browser, script)
// isn't counted - better too low than made-up numbers.
async function countDownload(req, env, what) {
  try {
    if (isBot(req) || req.headers.get("sec-fetch-mode") !== "navigate") return;
    const pf = (req.headers.get("sec-purpose") || req.headers.get("purpose") || "").toLowerCase();
    if (pf.includes("prefetch") || pf.includes("prerender")) return;
    const h = (await sha256(`${utcDay()}|${ipOf(req)}|${req.headers.get("user-agent")}|dl:${what}|${env.ADMIN_KEY || "echo"}`)).slice(0, 24);
    const r = await env.DB.prepare("INSERT OR IGNORE INTO uv (day, h) VALUES (?, ?)").bind(utcDay(), h).run();
    if (((r && r.meta) ? r.meta.changes : r && r.changes) > 0) await bump(env, "dl:" + what);
  } catch (e) { console.log("download count error", e && e.message); }
}
async function cleanupStats(env) {
  await env.DB.prepare("DELETE FROM uv WHERE day < ?").bind(utcDay(now() - 2 * 86400)).run();
  // pins of share links that were removed or expired
  await env.DB.prepare("DELETE FROM pins WHERE share_id NOT IN (SELECT id FROM shares WHERE expires = 0 OR expires > ?)").bind(now()).run();
}
async function siteStats(env) {
  const days = [...Array(30)].map((_, i) => utcDay(now() - (29 - i) * 86400)); // oldest .. today
  const rows = ((await env.DB.prepare("SELECT day, k, n FROM stats WHERE day >= ?").bind(days[0]).all()).results) || [];
  const keys = {}; // key -> [today, 7 days, 30 days]
  const daily = days.map((day) => ({ day, visitors: 0, views: 0, downloads: 0 }));
  for (const r of rows) {
    const i = days.indexOf(r.day);
    if (i < 0) continue;
    const age = 29 - i, a = keys[r.k] || (keys[r.k] = [0, 0, 0]);
    if (age < 1) a[0] += r.n;
    if (age < 7) a[1] += r.n;
    a[2] += r.n;
    if (r.k === "visitors") daily[i].visitors += r.n;
    else if (r.k.startsWith("view:")) daily[i].views += r.n;
    else if (r.k.startsWith("dl:")) daily[i].downloads += r.n;
  }
  return { keys, daily };
}

// ----- light profiles: echoclip.link/u/<handle> (name, picture, bio, colour, pinned clips, followers) -----
// Uses the Echo Cloud account. Who follows whom stays private: pages only show counts. No feeds.
const HANDLE_RE = /^[a-z0-9_]{3,20}$/;
const RESERVED = new Set(["admin", "administrator", "echo", "echoclip", "echocloud", "support", "help", "api", "www", "staff", "mod",
  "moderator", "official", "team", "root", "system", "discord", "download", "downloads", "pricing", "privacy", "terms", "changelog",
  "settings", "profile", "profiles", "login", "signin", "signup", "account", "billing", "null", "undefined", "me", "you", "kimbo"]);
const PIN_MAX = 6;
const cleanText = (t, max) => String(t || "").replace(/[\u0000-\u0008\u000b-\u001f\u007f‪-‮⁦-⁩]/g, "").replace(/\s+\n/g, "\n").trim().slice(0, max);
const cleanColor = (c) => (/^#[0-9a-f]{6}$/i.test(String(c || "")) ? String(c).toLowerCase() : "#7c5cff");
const avatarUrl = (origin, p) => (p && p.avatar ? `${origin}/u/${p.handle}/avatar.jpg?v=${p.avatar}` : "");
async function profileOf(env, userId) {
  return env.DB.prepare("SELECT * FROM profiles WHERE user_id = ?").bind(userId).first();
}
function publicProfile(origin, p, extra = {}) {
  return { handle: p.handle, name: p.name || p.handle, bio: p.bio || "", color: p.color || "#7c5cff", avatar: avatarUrl(origin, p),
    url: `${origin}/u/${p.handle}`, ...extra };
}
async function profileMe(user, env, origin) {
  const p = await profileOf(env, user.id);
  const t = now();
  const shares = ((await env.DB.prepare(`SELECT s.id, s.title, s.key, s.created, s.expires, s.thumb, s.dur, p.pos FROM shares s
      LEFT JOIN pins p ON p.share_id = s.id AND p.user_id = s.user_id
      WHERE s.user_id = ? AND (s.expires = 0 OR s.expires > ?) ORDER BY s.created DESC LIMIT 200`).bind(user.id, t).all()).results) || [];
  const following = ((await env.DB.prepare(`SELECT p.* FROM follows f JOIN profiles p ON p.user_id = f.followee
      WHERE f.follower = ? ORDER BY f.created DESC LIMIT 500`).bind(user.id).all()).results) || [];
  const fc = await env.DB.prepare("SELECT COUNT(*) AS n FROM follows WHERE followee = ?").bind(user.id).first();
  return json({
    profile: p ? publicProfile(origin, p) : null,
    followers: (fc && fc.n) || 0,
    following: following.map((f) => publicProfile(origin, f)),
    shares: shares.map((x) => ({ id: x.id, title: x.title || x.key.split("/").pop().replace(/\.mp4$/i, ""), created: x.created, expires: x.expires,
      thumb: x.thumb ? `${origin}/s/${x.id}.jpg` : "", dur: x.dur || 0, pinned: x.pos != null, url: `${origin}/s/${x.id}` })),
    pin_max: PIN_MAX,
  });
}
async function profileSave(req, user, env, origin) {
  if (!(await allow(env, `prof:${user.id}`, 40, 3600))) return fail("Slow down a little - try again in a while", 429);
  const b = await body(req);
  const handle = String(b.handle || "").trim().toLowerCase().replace(/^@/, "");
  if (!HANDLE_RE.test(handle)) return fail("Your @name needs 3-20 letters, numbers or _");
  const old = await profileOf(env, user.id);
  // a reserved @name given by William (admin) can be kept when saving other changes
  const mine = old && old.handle === handle;
  if (!mine && (RESERVED.has(handle) || (await env.DB.prepare("SELECT 1 AS x FROM banned_handles WHERE handle = ?").bind(handle).first())))
    return fail("That @name is reserved - pick another");
  const taken = await env.DB.prepare("SELECT user_id FROM profiles WHERE handle = ?").bind(handle).first();
  if (taken && taken.user_id !== user.id) return fail("That @name is taken - pick another", 409);
  const name = cleanText(b.name, 32) || handle, bio = cleanText(b.bio, 160), color = cleanColor(b.color), t = now();
  if (old) await env.DB.prepare("UPDATE profiles SET handle = ?, name = ?, bio = ?, color = ?, updated = ? WHERE user_id = ?").bind(handle, name, bio, color, t, user.id).run();
  else await env.DB.prepare("INSERT INTO profiles (user_id, handle, name, bio, color, avatar, created, updated) VALUES (?, ?, ?, ?, ?, 0, ?, ?)").bind(user.id, handle, name, bio, color, t, t).run();
  return json({ ok: true, profile: publicProfile(origin, await profileOf(env, user.id)) });
}
async function profileAvatar(req, user, env, origin) {
  const p = await profileOf(env, user.id);
  if (!p) return fail("Make your profile first");
  if (!(await allow(env, `avatar:${user.id}`, 20, 3600))) return fail("Slow down a little - try again in a while", 429);
  const b = await body(req);
  if (b.remove) {
    await env.BUCKET.delete(`avatars/${user.id}.jpg`);
    await env.DB.prepare("UPDATE profiles SET avatar = 0 WHERE user_id = ?").bind(user.id).run();
    return json({ ok: true, avatar: "" });
  }
  let bytes;
  try { bytes = Uint8Array.from(atob(String(b.img || "")), (c) => c.charCodeAt(0)); } catch { return fail("That picture didn't come through"); }
  if (bytes.length < 100 || bytes.length > 300000 || bytes[0] !== 0xff || bytes[1] !== 0xd8) return fail("That picture didn't come through");
  await env.BUCKET.put(`avatars/${user.id}.jpg`, bytes, { httpMetadata: { contentType: "image/jpeg" } });
  const v = (p.avatar || 0) + 1;
  await env.DB.prepare("UPDATE profiles SET avatar = ?, updated = ? WHERE user_id = ?").bind(v, now(), user.id).run();
  return json({ ok: true, avatar: avatarUrl(origin, { ...p, avatar: v }) });
}
async function profilePin(req, user, env) {
  const b = await body(req), id = String(b.id || "");
  const s = await env.DB.prepare("SELECT id FROM shares WHERE id = ? AND user_id = ?").bind(id, user.id).first();
  if (!s) return fail("That share link isn't yours or is gone", 404);
  if (!b.on) { await env.DB.prepare("DELETE FROM pins WHERE user_id = ? AND share_id = ?").bind(user.id, id).run(); return json({ ok: true }); }
  const c = await env.DB.prepare(`SELECT COUNT(*) AS n, MAX(p.pos) AS m FROM pins p JOIN shares s ON s.id = p.share_id
      WHERE p.user_id = ? AND (s.expires = 0 OR s.expires > ?)`).bind(user.id, now()).first();
  if ((c.n || 0) >= PIN_MAX) return fail(`You can pin up to ${PIN_MAX} clips - unpin one first`);
  await env.DB.prepare("INSERT OR IGNORE INTO pins (user_id, share_id, pos) VALUES (?, ?, ?)").bind(user.id, id, (c.m || 0) + 1).run();
  return json({ ok: true });
}
async function followSet(req, user, env, origin) {
  if (!(await allow(env, `follow:${user.id}`, 120, 3600))) return fail("Slow down a little - try again in a while", 429);
  const b = await body(req), handle = String(b.handle || "").trim().toLowerCase().replace(/^@/, "").replace(/^.*\/u\//, "").replace(/\/.*$/, "");
  const p = await env.DB.prepare("SELECT * FROM profiles WHERE handle = ?").bind(handle).first();
  if (!p) return fail(`Nobody is called @${handle || "?"} on Echo`, 404);
  if (p.user_id === user.id) return fail("That's you!");
  if (b.on === false) await env.DB.prepare("DELETE FROM follows WHERE follower = ? AND followee = ?").bind(user.id, p.user_id).run();
  else await env.DB.prepare("INSERT OR IGNORE INTO follows (follower, followee, created) VALUES (?, ?, ?)").bind(user.id, p.user_id, now()).run();
  return json({ ok: true, profile: publicProfile(origin, p) });
}
async function profileDelete(user, env) {
  await env.BUCKET.delete(`avatars/${user.id}.jpg`);
  await env.DB.batch([
    env.DB.prepare("DELETE FROM pins WHERE user_id = ?").bind(user.id),
    env.DB.prepare("DELETE FROM follows WHERE follower = ? OR followee = ?").bind(user.id, user.id),
    env.DB.prepare("DELETE FROM profiles WHERE user_id = ?").bind(user.id),
  ]);
  return json({ ok: true });
}
async function avatarFile(env, handle) {
  const p = await env.DB.prepare("SELECT user_id, avatar FROM profiles WHERE handle = ?").bind(handle).first();
  const o = p && p.avatar ? await env.BUCKET.get(`avatars/${p.user_id}.jpg`) : null;
  if (!o) return new Response("no picture", { status: 404 });
  return new Response(o.body, { headers: { "content-type": "image/jpeg", "cache-control": "public, max-age=31536000, immutable", "x-content-type-options": "nosniff" } });
}

/// echoclip.link/u/<handle>: the public profile page
async function profilePage(req, env, handle) {
  const p = await env.DB.prepare("SELECT * FROM profiles WHERE handle = ?").bind(handle).first();
  if (!p) return notFound(req, env);
  const origin = new URL(req.url).origin, t = now();
  const fc = await env.DB.prepare("SELECT COUNT(*) AS n FROM follows WHERE followee = ?").bind(p.user_id).first();
  const pins = ((await env.DB.prepare(`SELECT s.* FROM pins x JOIN shares s ON s.id = x.share_id
      WHERE x.user_id = ? AND s.user_id = x.user_id AND (s.expires = 0 OR s.expires > ?) ORDER BY x.pos LIMIT ?`).bind(p.user_id, t, PIN_MAX).all()).results) || [];
  const color = cleanColor(p.color), name = p.name || p.handle, followers = (fc && fc.n) || 0;
  const av = p.avatar ? `/u/${p.handle}/avatar.jpg?v=${p.avatar}` : "";
  const avatar = av ? `<img class="av" src="${av}" alt="">` : `<span class="av">${esc(name[0].toUpperCase())}</span>`;
  const len = (d) => (d ? `${Math.floor(d / 60)}:${String(Math.round(d % 60)).padStart(2, "0")}` : "");
  const cards = pins.map((s) => {
    const title = (s.title || s.key.split("/").pop().replace(/\.mp4$/i, "")).trim() || "Clip", game = shareGame(s);
    return `<a class="clip" href="/s/${s.id}"><span class="th">${s.thumb ? `<img src="/s/${s.id}.jpg" alt="" loading="lazy">` : ""}<span class="play"><svg viewBox="0 0 24 24"><path d="M8 5v14l11-7z"/></svg></span>${s.dur ? `<b>${len(s.dur)}</b>` : ""}</span>
<span class="ct"><strong>${esc(title)}</strong>${game ? `<small>${esc(game)}</small>` : ""}</span></a>`;
  }).join("");
  const desc = p.bio || `${name} clips with Echo, the lightweight, ad-free game clipper.`;
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>${esc(name)} (@${esc(p.handle)}) - Echo</title>
<meta name="description" content="${esc(desc)}">
<meta name="theme-color" content="${color}">
<meta property="og:site_name" content="Echo">
<meta property="og:title" content="${esc(name)} (@${esc(p.handle)})">
<meta property="og:description" content="${esc(desc)}">
<meta property="og:type" content="profile">
<meta property="og:url" content="${origin}/u/${p.handle}">
<meta property="og:image" content="${av ? origin + av : origin + "/og.png"}">
<meta name="twitter:card" content="summary">
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<style>
*{box-sizing:border-box;margin:0}
body{min-height:100vh;background:#0d0d10;color:#f5f5f7;font:15px/1.5 "Segoe UI Variable Text","Segoe UI",system-ui,-apple-system,sans-serif;-webkit-font-smoothing:antialiased}
a{color:inherit;text-decoration:none}
header{max-width:960px;margin:0 auto;padding:18px 20px;display:flex;align-items:center;gap:12px}
.brand{display:flex;align-items:center;gap:9px;font-weight:700;font-size:17px}
.logo{width:32px;height:32px;border-radius:10px;background:linear-gradient(135deg,#8b6cff,#6a4cf5 55%,#4f7bff);display:grid;place-items:center}
.top{margin-left:auto;font-size:14px;color:#a0a0ab}.top:hover{color:#f5f5f7}
main{max-width:960px;margin:0 auto;padding:0 20px 40px}
.banner{height:150px;border-radius:18px;background:radial-gradient(500px 200px at 20% 0%,${color}cc,transparent 70%),radial-gradient(600px 220px at 90% 120%,${color}66,transparent 70%),#16161a;border:1px solid #26262c}
.head{display:flex;align-items:flex-end;gap:18px;padding:0 24px;margin-top:-52px;flex-wrap:wrap}
.av{width:112px;height:112px;border-radius:50%;object-fit:cover;border:5px solid #0d0d10;background:${color};display:grid;place-items:center;font-size:44px;font-weight:700;color:#fff;flex:none}
.who{padding-bottom:8px;min-width:0}
h1{font-family:"Segoe UI Variable Display","Segoe UI",system-ui,sans-serif;font-size:28px;line-height:1.15;word-break:break-word}
.handle{color:#a0a0ab;font-size:15px}
.meta{margin-left:auto;padding-bottom:12px;display:flex;gap:10px;align-items:center}
.count{color:#a0a0ab;font-size:14px}.count b{color:#f5f5f7}
.fbtn{height:38px;padding:0 18px;border-radius:11px;border:0;font:inherit;font-weight:600;color:#fff;background:${color};cursor:pointer}
.fbtn:hover{filter:brightness(1.1)}
.bio{margin:16px 24px 0;color:#c9c9d1;white-space:pre-wrap;max-width:640px}
h2{font-size:13px;text-transform:uppercase;letter-spacing:.08em;color:#6b6b76;margin:34px 0 12px}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(270px,1fr));gap:14px}
.clip{background:#16161a;border:1px solid #26262c;border-radius:14px;overflow:hidden;transition:transform .15s,border-color .15s}
.clip:hover{transform:translateY(-3px);border-color:${color}99}
.th{position:relative;display:block;aspect-ratio:16/9;background:linear-gradient(135deg,${color}33,#1e1e23)}
.th img{width:100%;height:100%;object-fit:cover;display:block}
.th b{position:absolute;right:8px;bottom:8px;font-size:12px;background:#000c;padding:1px 6px;border-radius:6px}
.play{position:absolute;left:50%;top:50%;width:46px;height:46px;margin:-23px 0 0 -23px;border-radius:50%;background:#0009;display:grid;place-items:center;opacity:0;transition:opacity .15s}
.clip:hover .play{opacity:1}.play svg{width:22px;height:22px;fill:#fff;margin-left:2px}
.ct{display:block;padding:10px 12px}.ct strong{display:block;font-size:14px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}.ct small{color:#6b6b76;font-size:12.5px}
.empty{color:#6b6b76}
.note{position:fixed;left:50%;bottom:24px;width:min(560px,calc(100vw - 32px));transform:translateX(-50%) translateY(20px);opacity:0;transition:.2s;background:#1e1e23;border:1px solid #26262c;border-radius:12px;padding:12px 16px;font-size:14px;color:#a0a0ab;max-width:92vw;box-shadow:0 10px 30px #000a}
.note.on{opacity:1;transform:translateX(-50%)}.note b{color:#f5f5f7}.note a{color:${color};font-weight:600}
footer{max-width:960px;margin:0 auto;padding:10px 20px 30px;color:#6b6b76;font-size:13px}footer a{color:#a0a0ab}
@media(max-width:640px){.head{padding:0 8px}.meta{margin-left:0;width:100%}.bio{margin:14px 8px 0}.top{display:none}}
</style></head><body>
<header><a class="brand" href="/"><span class="logo"><svg viewBox="0 0 256 256" width="20" height="20" aria-hidden="true"><circle cx="128" cy="128" r="80" fill="none" stroke="#fff" stroke-width="22" stroke-linecap="round" stroke-dasharray="390 503" transform="rotate(-200 128 128)"/><path d="M48 104 L58 70 L82 96 Z" fill="#fff" stroke="#fff" stroke-width="8" stroke-linejoin="round"/><path d="M106 86 L176 128 L106 170 Z" fill="#fff" stroke="#fff" stroke-width="12" stroke-linejoin="round"/></svg></span>Echo</a><a class="top" href="/download">Get Echo - free, no ads</a></header>
<main>
<div class="banner"></div>
<div class="head">${avatar}<div class="who"><h1>${esc(name)}</h1><div class="handle">@${esc(p.handle)}</div></div>
<div class="meta"><span class="count"><b>${followers}</b> follower${followers === 1 ? "" : "s"}</span><button class="fbtn" id="fbtn" data-h="${esc(p.handle)}">Follow</button></div></div>
${p.bio ? `<p class="bio">${esc(p.bio)}</p>` : ""}
<h2>Pinned clips</h2>
${cards ? `<div class="grid">${cards}</div>` : `<p class="empty">No pinned clips yet.</p>`}
</main>
<div class="note" id="note"><b>@${esc(p.handle)} copied.</b> To follow, open Echo → Settings → Profile and paste it under Following · <a href="/download">Get Echo</a></div>
<footer>Echo profile · <a href="/terms">Terms</a> · Report this profile: <a href="mailto:support@echoclip.link?subject=Report%20profile%20${esc(p.handle)}">support@echoclip.link</a></footer>
<script src="/profile.js" defer></script>
</body></html>`;
  return new Response(req.method === "HEAD" ? null : html, { headers: {
    "content-type": "text/html; charset=utf-8", "cache-control": "public, max-age=60",
    "content-security-policy": "default-src 'none'; script-src 'self'; img-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'",
    "x-content-type-options": "nosniff", "referrer-policy": "strict-origin-when-cross-origin",
  } });
}

/// Every admin route: the x-admin-key header must equal secret ADMIN_KEY (compared in constant time).
/// Wrong keys are limited to 10 tries per hour per network, so it can't be guessed; nothing is read or
/// changed before this passes. Returns null when allowed, else the error response.
async function adminDenied(req, env) {
  const key = req.headers.get("x-admin-key") || "", want = env.ADMIN_KEY || "";
  if (!(await allow(env, `admin:${ipOf(req)}`, 120, 3600))) return fail("slow down", 429);
  let diff = want.length < 20 || key.length !== want.length ? 1 : 0;
  for (let i = 0; i < want.length; i++) diff |= want.charCodeAt(i) ^ (key.charCodeAt(i) || 0);
  if (!diff) return null;
  if (!(await allow(env, `adminbad:${ipOf(req)}`, 10, 3600))) return fail("Too many wrong keys - try again in an hour", 429);
  return fail("Wrong admin key", 403);
}

/// William's stats page (echoclip.link/admin -> GET /admin/stats with header x-admin-key = secret ADMIN_KEY).
/// Storage numbers are the last counts Echo Cloud made per account (recounted whenever someone uses it).
async function adminStats(req, env) {
  const denied = await adminDenied(req, env); if (denied) return denied;
  const t = now(), d = 86400;
  const one = async (sql, ...v) => (await env.DB.prepare(sql).bind(...v).first()) || {};
  const all = async (sql, ...v) => ((await env.DB.prepare(sql).bind(...v).all()).results) || [];
  const users = await one(`SELECT COUNT(*) AS total,
      SUM(created > ?) AS d1, SUM(created > ?) AS d7, SUM(created > ?) AS d30,
      SUM(polar_sub IS NOT NULL) AS paid, SUM(used_bytes) AS used, SUM(over_since > 0) AS over FROM users`, t - d, t - 7 * d, t - 30 * d);
  const active = await one(`SELECT COUNT(DISTINCT CASE WHEN last_seen > ? THEN user_id END) AS d1,
      COUNT(DISTINCT CASE WHEN last_seen > ? THEN user_id END) AS d7, COUNT(DISTINCT CASE WHEN last_seen > ? THEN user_id END) AS d30 FROM sessions`,
    t - d, t - 7 * d, t - 30 * d);
  const byPlan = await all("SELECT plan, COUNT(*) AS n, SUM(used_bytes) AS used FROM users GROUP BY plan ORDER BY n DESC");
  const top = await all("SELECT email, plan, used_bytes AS used, quota_bytes AS quota, used_at, over_since, created FROM users ORDER BY used_bytes DESC LIMIT 15");
  const newest = await all("SELECT email, plan, created FROM users ORDER BY created DESC LIMIT 10");
  const over = await all("SELECT email, used_bytes AS used, quota_bytes AS quota, over_since, over_stage FROM users WHERE over_since > 0 ORDER BY over_since LIMIT 20");
  const shares = await one("SELECT COUNT(*) AS total, SUM(created > ?) AS d7, SUM(expires = 0 OR expires > ?) AS live FROM shares", t - 7 * d, t);
  const profs = await one("SELECT COUNT(*) AS n, SUM(created > ?) AS d7 FROM profiles", t - 7 * d);
  const fl = await one("SELECT COUNT(*) AS n FROM follows");
  const profList = await all(`SELECT p.handle, p.name, p.created, u.email, (SELECT COUNT(*) FROM follows f WHERE f.followee = p.user_id) AS followers,
      (SELECT COUNT(*) FROM pins x WHERE x.user_id = p.user_id) AS pins FROM profiles p LEFT JOIN users u ON u.id = p.user_id ORDER BY p.created DESC LIMIT 100`);
  // income: what each subscription really pays per month after discounts (stored by the Polar webhook);
  // subscriptions from before that (sub_cents -1) are counted as "unknown", not guessed
  const paidRows = await all("SELECT sub_cents, sub_currency FROM users WHERE polar_sub IS NOT NULL");
  let gross = 0, cur = "EUR", paying = 0, free = 0, unknown = 0;
  for (const u of paidRows) {
    if (u.sub_cents < 0) { unknown++; continue; }
    if (u.sub_cents === 0) { free++; continue; }
    gross += u.sub_cents / 100; paying++; cur = (u.sub_currency || cur).toUpperCase();
  }
  // Polar keeps 5% + 0.50 per payment; prices include VAT, which Polar pays to the tax office (~20% in the EU)
  const net = Math.max(0, gross / 1.2 * 0.95 - 0.5 * paying);
  const usedGB = (users.used || 0) / GB;
  const r2 = Math.max(0, usedGB - 10) * 0.015;   // R2: first 10 GB free, then $0.015 per GB-month
  const domain = 7.2 / 12;
  let logs = 0;
  try { logs = (await env.BUCKET.list({ prefix: "logs/", limit: 1000 })).objects.length; } catch { /* ignore */ }
  let backup = null;
  try { const b = await env.BUCKET.list({ prefix: "backups/db-" }); backup = b.objects.map((o) => o.key).sort().pop() || null; } catch { /* ignore */ }
  return json({
    at: t,
    users: { total: users.total || 0, new_1d: users.d1 || 0, new_7d: users.d7 || 0, new_30d: users.d30 || 0, paid: users.paid || 0, over: users.over || 0 },
    active: { d1: active.d1 || 0, d7: active.d7 || 0, d30: active.d30 || 0 },
    plans: byPlan,
    storage: { used_bytes: users.used || 0 },
    money: { currency: cur, gross_month: +gross.toFixed(2), net_month: +net.toFixed(2), paying, discounted_free: free, unknown, r2_usd_month: +r2.toFixed(2), domain_usd_month: +domain.toFixed(2) },
    shares: { total: shares.total || 0, live: shares.live || 0, new_7d: shares.d7 || 0 },
    logs, backup, top, newest, over,
    profiles: { total: profs.n || 0, new_7d: profs.d7 || 0, follows: fl.n || 0, list: profList },
    site: await siteStats(env),
  });
}

/// William removes a reported profile: POST /admin/profile-takedown {handle, block?} with header x-admin-key.
/// Removes the page, picture, pins and follows (clips and share links stay); block = the @name can't be used again.
async function profileTakedown(req, env) {
  const denied = await adminDenied(req, env); if (denied) return denied;
  const b = await body(req);
  const handle = String(b.handle || "").trim().toLowerCase().replace(/^.*\/u\//, "").replace(/^@/, "").replace(/\/.*$/, "");
  const p = await env.DB.prepare("SELECT * FROM profiles WHERE handle = ?").bind(handle).first();
  if (b.block && HANDLE_RE.test(handle)) await env.DB.prepare("INSERT OR IGNORE INTO banned_handles (handle, at) VALUES (?, ?)").bind(handle, now()).run();
  if (!p) return b.block ? json({ ok: true, handle, removed: false, blocked: true }) : fail("No profile with that @name", 404);
  const owner = await env.DB.prepare("SELECT email FROM users WHERE id = ?").bind(p.user_id).first();
  await profileDelete({ id: p.user_id }, env);
  console.log("profile takedown", handle, b.block ? "(blocked)" : "");
  return json({ ok: true, handle, removed: true, blocked: !!b.block, owner: owner ? owner.email : null });
}

/// William changes someone's @name: POST /admin/profile-handle {who, to, block_old?} with header x-admin-key.
/// who = their @name, /u/ link or account email; to = the new @name (reserved names allowed, e.g. "echo" for
/// William himself) or "" = a random one (user_123456) they can change later. An account without a profile gets
/// one when `to` is given. block_old = nobody can take the old @name again (inappropriate names).
async function profileHandle(req, env) {
  const denied = await adminDenied(req, env); if (denied) return denied;
  const b = await body(req);
  const who = String(b.who || "").trim();
  let p = null, userId = null;
  if (who.includes("@") && who.indexOf("@") > 0) {
    const u = await env.DB.prepare("SELECT id FROM users WHERE email = ?").bind(who.toLowerCase()).first();
    if (!u) return fail("No account with that email", 404);
    userId = u.id; p = await profileOf(env, u.id);
  } else {
    const h = who.toLowerCase().replace(/^.*\/u\//, "").replace(/^@/, "").replace(/\/.*$/, "");
    p = await env.DB.prepare("SELECT * FROM profiles WHERE handle = ?").bind(h).first();
    if (!p) return fail("No profile with that @name", 404);
    userId = p.user_id;
  }
  let to = String(b.to || "").trim().toLowerCase().replace(/^@/, "");
  if (to) {
    if (!HANDLE_RE.test(to)) return fail("The new @name needs 3-20 letters, numbers or _");
    const taken = await env.DB.prepare("SELECT user_id FROM profiles WHERE handle = ?").bind(to).first();
    if (taken && taken.user_id !== userId) return fail(`@${to} belongs to someone else - change theirs first`, 409);
  } else {
    if (!p) return fail("That account has no profile - give a new @name to make one");
    for (let i = 0; i < 20 && !to; i++) {
      const c = "user_" + String(100000 + Math.floor(Math.random() * 900000));
      if (!(await env.DB.prepare("SELECT 1 AS x FROM profiles WHERE handle = ?").bind(c).first())) to = c;
    }
    if (!to) return fail("Couldn't find a free random @name - try again", 500);
  }
  const t = now(), from = p ? p.handle : null;
  // random name (usually an inappropriate one): the display name goes too; otherwise it only follows when it was the @name
  const resetName = !String(b.to || "").trim() ? 1 : 0;
  if (p) await env.DB.prepare("UPDATE profiles SET handle = ?, name = CASE WHEN name = handle OR ? = 1 THEN ? ELSE name END, updated = ? WHERE user_id = ?").bind(to, resetName, to, t, userId).run();
  else await env.DB.prepare("INSERT INTO profiles (user_id, handle, name, bio, color, avatar, created, updated) VALUES (?, ?, ?, '', '#7c5cff', 0, ?, ?)").bind(userId, to, to, t, t).run();
  if (b.block_old && from && from !== to) await env.DB.prepare("INSERT OR IGNORE INTO banned_handles (handle, at) VALUES (?, ?)").bind(from, t).run();
  const owner = await env.DB.prepare("SELECT email FROM users WHERE id = ?").bind(userId).first();
  console.log("profile handle", from, "->", to, b.block_old ? "(old blocked)" : "");
  return json({ ok: true, from, to, blocked: !!(b.block_old && from), owner: owner ? owner.email : null });
}

/// William removes a reported link: POST /admin/takedown {id, delete_clip?} with header x-admin-key
async function takedown(req, env) {
  const denied = await adminDenied(req, env); if (denied) return denied;
  const b = await body(req);
  const id = String(b.id || "").replace(/^.*\/s\//, "").replace(/\.(mp4|jpg)$/, "").trim();
  const row = await env.DB.prepare("SELECT * FROM shares WHERE id = ?").bind(id).first();
  if (!row) return fail("No share link with that id", 404);
  const owner = await env.DB.prepare("SELECT id, email FROM users WHERE id = ?").bind(row.user_id).first();
  await env.BUCKET.delete(`thumbs/${id}.jpg`);
  await env.DB.prepare("DELETE FROM shares WHERE id = ?").bind(id).run();
  if (b.delete_clip) { await env.BUCKET.delete(row.key); await env.DB.prepare("DELETE FROM shares WHERE key = ?").bind(row.key).run(); }
  console.log("takedown", id, row.key, b.delete_clip ? "(clip deleted)" : "");
  return json({ ok: true, id, clip: row.key, owner: owner ? owner.email : null, clip_deleted: !!b.delete_clip });
}

// ----- share links: /s/<id> streams a clip straight from R2 (plays in Discord / any browser) -----
let migrated = false;
async function migrate(env) {
  if (migrated) return;
  await env.DB.batch([
    env.DB.prepare("CREATE TABLE IF NOT EXISTS shares (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, key TEXT NOT NULL, created INTEGER NOT NULL, expires INTEGER NOT NULL DEFAULT 0)"),
    env.DB.prepare("CREATE INDEX IF NOT EXISTS shares_key ON shares (key)"),
    env.DB.prepare("CREATE TABLE IF NOT EXISTS limits (k TEXT PRIMARY KEY, n INTEGER NOT NULL, reset INTEGER NOT NULL)"),
    // upload links handed out but not finished yet (they count toward the plan until done or expired)
    env.DB.prepare("CREATE TABLE IF NOT EXISTS uploads (key TEXT PRIMARY KEY, user_id TEXT NOT NULL, size INTEGER NOT NULL, expires INTEGER NOT NULL)"),
    env.DB.prepare("CREATE TABLE IF NOT EXISTS stats (day TEXT NOT NULL, k TEXT NOT NULL, n INTEGER NOT NULL, PRIMARY KEY (day, k))"),
    // light profiles (chat 19): a name, picture, bio and colour, up to 6 pinned share links, and who you follow
    env.DB.prepare("CREATE TABLE IF NOT EXISTS profiles (user_id TEXT PRIMARY KEY, handle TEXT NOT NULL UNIQUE, name TEXT, bio TEXT, color TEXT, avatar INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL)"),
    env.DB.prepare("CREATE TABLE IF NOT EXISTS pins (user_id TEXT NOT NULL, share_id TEXT NOT NULL, pos INTEGER NOT NULL, PRIMARY KEY (user_id, share_id))"),
    env.DB.prepare("CREATE TABLE IF NOT EXISTS follows (follower TEXT NOT NULL, followee TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY (follower, followee))"),
    env.DB.prepare("CREATE INDEX IF NOT EXISTS follows_followee ON follows (followee)"),
    // @names William removed and blocked (can't be taken again)
    env.DB.prepare("CREATE TABLE IF NOT EXISTS banned_handles (handle TEXT PRIMARY KEY, at INTEGER NOT NULL)"),
    env.DB.prepare("CREATE TABLE IF NOT EXISTS uv (day TEXT NOT NULL, h TEXT NOT NULL, PRIMARY KEY (day, h))"),
    env.DB.prepare("DELETE FROM limits WHERE reset < ?").bind(now() - 86400),
    // quotas used to be counted in GiB (10 GB showed as 10.74 GB)
    env.DB.prepare("UPDATE users SET quota_bytes = ? WHERE plan = 'free' AND quota_bytes = ?").bind(Number(env.FREE_GB || 10) * GB, Number(env.FREE_GB || 10) * 1024 ** 3),
  ]);
  // paid plans (Polar): which subscription gave the quota, and until when it's paid
  const cols = new Set(((await env.DB.prepare("PRAGMA table_info(users)").all()).results || []).map((c) => c.name));
  for (const [c, type] of [["polar_sub", "TEXT"], ["plan_until", "INTEGER NOT NULL DEFAULT 0"], ["polar_at", "INTEGER NOT NULL DEFAULT 0"],
    // over the plan: since when (0 = not), which email went out (1 = warning, 2 = reminder, 3 = removing), files removed so far
    ["over_since", "INTEGER NOT NULL DEFAULT 0"], ["over_stage", "INTEGER NOT NULL DEFAULT 0"], ["over_removed", "INTEGER NOT NULL DEFAULT 0"],
    // what the subscription really pays per month after discounts, in cents (-1 = not known yet: older subscription)
    ["sub_cents", "INTEGER NOT NULL DEFAULT -1"], ["sub_currency", "TEXT"]]) {
    if (!cols.has(c)) await env.DB.prepare(`ALTER TABLE users ADD COLUMN ${c} ${type}`).run();
  }
  // share pages (Discord / browser embeds): clip title, size, length and whether a thumbnail was uploaded
  const scols = new Set(((await env.DB.prepare("PRAGMA table_info(shares)").all()).results || []).map((c) => c.name));
  for (const [c, type] of [["title", "TEXT"], ["w", "INTEGER NOT NULL DEFAULT 0"], ["h", "INTEGER NOT NULL DEFAULT 0"], ["dur", "REAL NOT NULL DEFAULT 0"], ["thumb", "INTEGER NOT NULL DEFAULT 0"], ["wm", "TEXT"],
    // how often the share page's video was played (once per viewer per day; Discord's inline player can't be counted)
    ["views", "INTEGER NOT NULL DEFAULT 0"]]) {
    if (!scols.has(c)) await env.DB.prepare(`ALTER TABLE shares ADD COLUMN ${c} ${type}`).run();
  }
  migrated = true;
}

async function share(req, user, env, origin) {
  const b = await body(req);
  const path = String(b.path || "").replace(/^\/+/, "");
  if (!path || path.length > 1000 || path.split("/").some((p) => p === ".." || p === "")) return fail("Bad clip path");
  const key = `u/${user.id}/${path}`;
  if (!(await env.BUCKET.head(key))) return fail("This clip isn't in Echo Cloud yet - upload it first", 404);
  const days = Math.max(0, Math.min(3650, Number(b.days) || 0));
  const expires = days ? now() + days * 86400 : 0;
  const old = await env.DB.prepare("SELECT id FROM shares WHERE key = ? AND user_id = ?").bind(key, user.id).first();
  let id = old && old.id;
  if (id) {
    await env.DB.prepare("UPDATE shares SET expires = ? WHERE id = ?").bind(expires, id).run();
  } else {
    id = randomId();
    await env.DB.prepare("INSERT INTO shares (id, user_id, key, created, expires) VALUES (?, ?, ?, ?, ?)")
      .bind(id, user.id, key, now(), expires).run();
  }
  // what the share page and Discord show: name, video size, length and a small thumbnail (all optional)
  const title = String(b.title || "").replace(/[\u0000-\u001f]/g, " ").trim().slice(0, 120);
  const w = Math.max(0, Math.min(8192, Math.round(Number(b.width) || 0)));
  const h = Math.max(0, Math.min(8192, Math.round(Number(b.height) || 0)));
  const dur = Math.max(0, Math.min(86400, Number(b.duration) || 0));
  let thumb = 0;
  if (typeof b.thumb === "string" && b.thumb.length < 400000) {
    try {
      const bytes = Uint8Array.from(atob(b.thumb), (c) => c.charCodeAt(0));
      if (bytes[0] === 0xff && bytes[1] === 0xd8) {   // a JPEG
        await env.BUCKET.put(`thumbs/${id}.jpg`, bytes, { httpMetadata: { contentType: "image/jpeg" } });
        thumb = 1;
      }
    } catch { /* bad base64: no thumbnail */ }
  }
  // Echo watermark the page lays over the video ("kimbo" / "link" / "echo"; none = no logo). The clip itself stays as uploaded.
  const wm = b.wm === "kimbo" || b.wm === "echo" || b.wm === "link" ? b.wm : null;
  await env.DB.prepare("UPDATE shares SET title = ?, w = ?, h = ?, dur = ?, thumb = MAX(thumb, ?), wm = ? WHERE id = ?")
    .bind(title || null, w, h, dur, thumb, wm, id).run();
  return json({ url: `${origin}/s/${id}`, expires });
}

const esc = (t) => String(t).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const BOTS = /discordbot|twitterbot|facebookexternalhit|slackbot|telegrambot|whatsapp|linkedinbot|redditbot|skypeuripreview|embedly|mastodon/i;

/// /s/<id> in a browser or a chat app's link preview: a small player page with Open Graph tags
/// (Discord plays og:video inline, like Medal links). Video players and downloads use /s/<id>.mp4.
/// the game of a shared clip: its folder in Echo (Echo/<year>/<month>/<day>/<game>/<clip>.mp4), if it isn't a date
function shareGame(row) {
  const parts = row.key.split("/"), folder = parts.length > 2 ? parts[parts.length - 2] : "";
  return folder && !folder.startsWith(".") && !/^\d|^(january|february|march|april|may|june|july|august|september|october|november|december)\b/i.test(folder) && folder !== "Echo" ? folder : "";
}
/// oEmbed for a share link: Discord shows provider_name as the small line on top and author_name under it
/// (like Medal's "Medal - Record what just happened…" / "player · game")
async function shareOembed(req, env, id) {
  const row = await env.DB.prepare("SELECT * FROM shares WHERE id = ?").bind(id).first();
  if (!row || (row.expires && row.expires < now())) return json({ error: "gone" }, 404);
  const origin = new URL(req.url).origin, prof = await profileOf(env, row.user_id);
  const name = (row.title || row.key.split("/").pop().replace(/\.mp4$/i, "")).trim() || "Echo clip";
  return new Response(JSON.stringify({
    version: "1.0", type: "link", title: name,
    // one line on top (William: two lines was too much); clicking it opens the download page
    // with a profile the same line credits the clipper and links to their page
    provider_name: prof ? `${prof.name || prof.handle} (@${prof.handle}) on Echo · lightweight, ad-free clipping` : "Echo · lightweight, ad-free game clipping",
    provider_url: prof ? `${origin}/u/${prof.handle}` : `${origin}/download`,
  }), { headers: { "content-type": "application/json", "cache-control": "public, max-age=3600", "access-control-allow-origin": "*" } });
}

/// POST /s/<id>/view from player.js when someone starts the video on the share page: one view per viewer
/// (IP + browser, scrambled, kept 2 days) per clip per day; bots and link previews don't count
async function shareView(req, env, id) {
  const done = new Response(null, { status: 204 });
  try {
    if (isBot(req) || !(await allow(env, `view:${ipOf(req)}`, 120, 3600))) return done;
    const h = (await sha256(`${utcDay()}|${ipOf(req)}|${req.headers.get("user-agent")}|v:${id}|${env.ADMIN_KEY || "echo"}`)).slice(0, 24);
    const r = await env.DB.prepare("INSERT OR IGNORE INTO uv (day, h) VALUES (?, ?)").bind(utcDay(), h).run();
    if (((r && r.meta) ? r.meta.changes : r && r.changes) > 0)
      await env.DB.prepare("UPDATE shares SET views = views + 1 WHERE id = ? AND (expires = 0 OR expires > ?)").bind(id, now()).run();
  } catch (e) { console.log("view count error", e && e.message); }
  return done;
}
/// POST /shares/views {ids} -> {views: {id: n}} for the caller's own links (Echo shows them on its clip cards)
async function shareViews(req, user, env) {
  const b = await body(req);
  const ids = (Array.isArray(b.ids) ? b.ids : []).map(String).filter((x) => /^[a-z0-9]{6,20}$/.test(x)).slice(0, 500);
  const views = {};
  for (let i = 0; i < ids.length; i += 90) {
    const part = ids.slice(i, i + 90);
    const rows = ((await env.DB.prepare(`SELECT id, views FROM shares WHERE user_id = ? AND id IN (${part.map(() => "?").join(",")})`)
      .bind(user.id, ...part).all()).results) || [];
    for (const r of rows) views[r.id] = r.views || 0;
  }
  return json({ views });
}
function sharePage(req, row, id, prof) {
  const u = new URL(req.url), base = `${u.origin}/s/${id}`;
  const name = (row.title || row.key.split("/").pop().replace(/\.mp4$/i, "")).trim() || "Echo clip";
  const w = row.w || 1920, h = row.h || 1080;
  const mins = row.dur ? `${Math.floor(row.dur / 60)}:${String(Math.round(row.dur % 60)).padStart(2, "0")}` : "";
  const img = row.thumb ? `${base}.jpg` : `${u.origin}/og.png`;
  const game = shareGame(row);
  const desc = [mins, game, "Clipped with Echo, the lightweight clipping app"].filter(Boolean).join(" · ");
  const logo = `<svg viewBox="0 0 256 256" width="20" height="20" aria-hidden="true"><circle cx="128" cy="128" r="80" fill="none" stroke="#fff" stroke-width="22" stroke-linecap="round" stroke-dasharray="390 503" transform="rotate(-200 128 128)"/><path d="M48 104 L58 70 L82 96 Z" fill="#fff" stroke="#fff" stroke-width="8" stroke-linejoin="round"/><path d="M106 86 L176 128 L106 170 Z" fill="#fff" stroke="#fff" stroke-width="12" stroke-linejoin="round"/></svg>`;
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>${esc(name)} - Echo</title>
<meta name="description" content="${esc(desc)}">
<meta name="theme-color" content="#7c5cff">
<meta property="og:site_name" content="Echo">
<meta property="og:title" content="${esc(name)}">
<meta property="og:description" content="${esc(desc)}">
<meta property="og:type" content="video.other">
<meta property="og:url" content="${base}">
<meta property="og:image" content="${img}">
<meta property="og:video" content="${base}.mp4">
<meta property="og:video:url" content="${base}.mp4">
<meta property="og:video:secure_url" content="${base}.mp4">
<meta property="og:video:type" content="video/mp4">
<meta property="og:video:width" content="${w}">
<meta property="og:video:height" content="${h}">
<meta name="twitter:card" content="player">
<meta name="twitter:title" content="${esc(name)}">
<meta name="twitter:image" content="${img}">
<meta name="twitter:player" content="${base}.mp4">
<meta name="twitter:player:width" content="${w}">
<meta name="twitter:player:height" content="${h}">
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="alternate" type="application/json+oembed" href="${u.origin}/oembed/${id}" title="${esc(name)}">
<style>
*{box-sizing:border-box;margin:0}
body{min-height:100vh;background:radial-gradient(1000px 420px at 50% -160px,#6a4cf540,transparent 70%),#0d0d10;color:#f5f5f7;font:15px/1.5 "Segoe UI Variable Text","Segoe UI",system-ui,-apple-system,sans-serif;-webkit-font-smoothing:antialiased}
a{color:#9d85ff;text-decoration:none}a:hover{text-decoration:underline}
header{max-width:1180px;margin:0 auto;padding:18px 20px;display:flex;align-items:center;gap:12px}
.brand{display:flex;align-items:center;gap:9px;color:#f5f5f7;font-weight:700;font-size:17px}.brand:hover{text-decoration:none}
.logo{width:32px;height:32px;border-radius:10px;background:linear-gradient(135deg,#8b6cff,#6a4cf5 55%,#4f7bff);display:grid;place-items:center;box-shadow:0 6px 18px #6a4cf555}
.top{margin-left:auto;font-size:14px;color:#a0a0ab}
main{max-width:1180px;margin:0 auto;padding:4px 20px 40px}
.player{position:relative;border-radius:16px;overflow:hidden;background:#000;border:1px solid #26262c;box-shadow:0 30px 80px #000a,0 0 0 1px #7c5cff22}
video{width:100%;max-height:76vh;display:block;background:#000}
.wm{position:absolute;right:2.5%;bottom:3.5%;height:7.5%;max-height:64px;pointer-events:none;user-select:none}
.player:fullscreen .wm,.player:-webkit-full-screen .wm{max-height:none}
.info{display:flex;gap:16px;flex-wrap:wrap;align-items:flex-start;justify-content:space-between;margin-top:18px}
h1{font-family:"Segoe UI Variable Display","Segoe UI",system-ui,sans-serif;font-size:26px;line-height:1.2;letter-spacing:-.01em;word-break:break-word}
.chips{display:flex;gap:8px;flex-wrap:wrap;margin-top:8px}
.chip{font-size:12.5px;font-weight:600;padding:3px 10px;border-radius:99px;background:#16161a;border:1px solid #26262c;color:#a0a0ab}
.chip.g{color:#9d85ff;background:#7c5cff1a;border-color:#7c5cff4d}
.acts{display:flex;gap:8px}
.btn{display:inline-flex;align-items:center;gap:8px;height:40px;padding:0 16px;border-radius:11px;font-weight:600;border:1px solid #26262c;background:#1e1e23;color:#f5f5f7;transition:transform .15s,filter .15s}
.btn svg{width:17px;height:17px;fill:none;stroke:currentColor;stroke-width:2;stroke-linecap:round;stroke-linejoin:round}
.btn.p{background:linear-gradient(135deg,#8b6cff,#6a4cf5 55%,#4f7bff);border-color:transparent;box-shadow:0 8px 24px #6a4cf540}
.btn:hover{text-decoration:none;transform:translateY(-1px);filter:brightness(1.1)}
.promo{margin-top:34px;display:flex;gap:18px;align-items:center;flex-wrap:wrap;background:linear-gradient(160deg,#7c5cff1a,#16161a 60%);border:1px solid #7c5cff40;border-radius:16px;padding:20px 22px}
.promo .logo{width:46px;height:46px;border-radius:13px;flex:none}.promo .logo svg{width:28px;height:28px}
.promo b{display:block;font-size:16.5px}.promo p{color:#a0a0ab;font-size:14px;margin-top:2px}
.promo .btn{margin-left:auto}
.pts{display:flex;gap:10px;flex-wrap:wrap;margin-top:6px;color:#6b6b76;font-size:13px}.pts span+span::before{content:"·";margin-right:10px}
footer{max-width:1180px;margin:0 auto;padding:10px 20px 30px;color:#6b6b76;font-size:13px}
.ep video{cursor:pointer}
.epui{position:absolute;inset:0;pointer-events:none}
.epbig{position:absolute;left:50%;top:50%;width:76px;height:76px;margin:-38px 0 0 -38px;border-radius:50%;background:#7c5cffd9;display:grid;place-items:center;pointer-events:auto;cursor:pointer;opacity:0;transform:scale(.85);transition:opacity .2s,transform .2s;box-shadow:0 10px 40px #6a4cf580}
.epbig svg{width:34px;height:34px;fill:#fff;margin-left:4px}
.ep.paused .epbig{opacity:1;transform:none}
.epbar{position:absolute;left:0;right:0;bottom:0;padding:34px 16px 10px;background:linear-gradient(transparent,#000c);pointer-events:auto;transition:opacity .25s}
.ep.idle .epbar{opacity:0}.ep.idle{cursor:none}
.eptl{position:relative;height:18px;display:flex;align-items:center;cursor:pointer;touch-action:none;outline:none}
.eptrack{position:relative;width:100%;height:6px;border-radius:6px;background:#ffffff33;overflow:hidden;transition:height .12s}
.eptl:hover .eptrack,.ep.seeking .eptrack{height:9px}
.eptrack i{position:absolute;inset:0;transform-origin:0 50%;transform:scaleX(0)}
.epbuf{background:#ffffff40}.epdone{background:linear-gradient(90deg,#8b6cff,#7c5cff 60%,#9d85ff)}
.epknob{position:absolute;top:50%;width:16px;height:16px;margin:-8px 0 0 -8px;border-radius:50%;background:#fff;box-shadow:0 0 0 4px #7c5cff66,0 2px 8px #0008;transform:scale(0);transition:transform .12s}
.eptl:hover .epknob,.ep.seeking .epknob,.eptl:focus-visible .epknob{transform:scale(1)}
.eptip{position:absolute;bottom:22px;transform:translateX(-50%);background:#16161ae6;border:1px solid #26262c;border-radius:6px;padding:2px 7px;font-size:12px;font-weight:600;opacity:0;pointer-events:none;transition:opacity .12s;white-space:nowrap}
.eptl:hover .eptip{opacity:1}
.eprow{display:flex;align-items:center;gap:6px;margin-top:4px}
.epb{width:38px;height:38px;border:0;border-radius:10px;background:none;color:#fff;display:grid;place-items:center;cursor:pointer;transition:background .15s}
.epb:hover{background:#ffffff1f}.epb svg{width:22px;height:22px;fill:#fff}
.epb svg .w{fill:none;stroke:#fff;stroke-width:2;stroke-linecap:round;stroke-linejoin:round}
.epvol{display:flex;align-items:center}.epvol input{width:0;opacity:0;transition:width .2s,opacity .2s;accent-color:#7c5cff;cursor:pointer}
.epvol:hover input,.epvol input:focus{width:84px;opacity:1;margin:0 6px}
.eptime{font-size:13.5px;font-weight:600;font-variant-numeric:tabular-nums;color:#f5f5f7;margin-left:4px}
.epgrow{flex:1}
.ep:fullscreen{border-radius:0;border:0}.ep:fullscreen video{max-height:none;height:100vh}
.ep.touch .epbig{opacity:0;pointer-events:none;z-index:2;width:60px;height:60px;margin:-30px 0 0 -30px}.ep.touch .epbig svg{width:28px;height:28px}.ep.touch .epbar{padding-top:18px}
.ep.touch:not(.idle) .epbig,.ep.touch.paused .epbig{opacity:1;transform:none;pointer-events:auto}
.ep.touch .epbig svg{margin-left:0}.ep.touch.paused .epbig svg{margin-left:4px}
.ep.touch.idle .epui{pointer-events:none}.ep.touch.idle .epbar{pointer-events:none}
.ep.touch .eptrack{height:8px}.ep.touch .epknob{transform:scale(1)}
.ep.touch .epvol input{display:none}.ep.touch .eptip{display:none}
.ep:-webkit-full-screen{border-radius:0;border:0;width:100%;height:100%}
.ep:-webkit-full-screen video{max-height:none;height:100vh}
.by{display:inline-flex;align-items:center;gap:8px;margin-top:8px;color:#f5f5f7;font-weight:600;font-size:14px}.by:hover{text-decoration:none}.by:hover span{text-decoration:underline}
.by img,.by i{width:26px;height:26px;border-radius:50%;object-fit:cover;flex:none}.by i{display:grid;place-items:center;font-style:normal;font-size:12px;color:#fff}.by small{color:#a0a0ab;font-weight:500}
@media(max-width:640px){h1{font-size:21px}.acts{width:100%}.acts .btn{flex:1;justify-content:center}.promo .btn{margin-left:0;width:100%;justify-content:center}.top{display:none}}
</style></head><body>
<header><a class="brand" href="https://echoclip.link/"><span class="logo">${logo}</span>Echo</a><span class="top">Lightweight game clipping for any PC</span></header>
<main>
<div class="player"><video src="${base}.mp4" ${row.thumb ? `poster="${base}.jpg"` : ""} controls autoplay playsinline preload="metadata"></video>${row.wm === "kimbo" || row.wm === "echo" || row.wm === "link" ? `<img class="wm" src="/wm/${row.wm}.png" alt="">` : ""}</div>
<div class="info"><div><h1>${esc(name)}</h1>${prof ? `<a class="by" href="/u/${prof.handle}">${prof.avatar ? `<img src="/u/${prof.handle}/avatar.jpg?v=${prof.avatar}" alt="">` : `<i style="background:${cleanColor(prof.color)}">${esc((prof.name || prof.handle)[0].toUpperCase())}</i>`}<span>${esc(prof.name || prof.handle)} <small>@${esc(prof.handle)}</small></span></a>` : ""}<div class="chips">${mins ? `<span class="chip">${mins}</span>` : ""}${game ? `<span class="chip g">${esc(game)}</span>` : ""}${row.views > 0 ? `<span class="chip">${row.views} view${row.views === 1 ? "" : "s"}</span>` : ""}<span class="chip">Shared with Echo</span></div></div>
<div class="acts"><a class="btn" href="${base}.mp4" download><svg viewBox="0 0 24 24"><path d="M12 4v11m0 0-4-4m4 4 4-4M5 20h14"/></svg>Download</a></div></div>
<div class="promo"><span class="logo">${logo.replace('width="20" height="20"', 'width="28" height="28"')}</span>
<div><b>Clip your own moments with Echo</b><p>Free and light enough for low-end PCs. Press one key to save what just happened.</p>
<div class="pts"><span>No ads</span><span>No account needed</span><span>Copy straight into Discord</span><span>Windows &amp; Linux</span></div></div>
<a class="btn p" href="https://echoclip.link/download">Get Echo, it's free</a></div>
</main>
<footer>Shared with <a href="https://echoclip.link/">Echo</a> · <a href="https://echoclip.link/terms">Terms</a> · Report this clip: <a href="mailto:support@echoclip.link?subject=Report%20${id}">support@echoclip.link</a></footer>
<script src="/player.js" defer></script>
</body></html>`;
  return new Response(req.method === "HEAD" ? null : html, { headers: {
    "content-type": "text/html; charset=utf-8", "cache-control": "public, max-age=300",
    // only our own player script (/player.js); the video and poster come from this site
    "content-security-policy": "default-src 'none'; script-src 'self'; connect-src 'self'; media-src 'self'; img-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'",
    "x-content-type-options": "nosniff", "referrer-policy": "no-referrer",
  } });
}


// ----- paid plans through Polar (merchant of record: handles payment, VAT, invoices, cancelling) -----
async function polar(env, method, path, b) {
  if (!env.POLAR_TOKEN) throw new Error("POLAR_TOKEN missing");
  const r = await fetch(`${env.POLAR_API || "https://api.polar.sh"}${path}`, {
    method,
    headers: { authorization: `Bearer ${env.POLAR_TOKEN}`, "content-type": "application/json", accept: "application/json" },
    body: b ? JSON.stringify(b) : undefined,
  });
  const t = await r.text();
  if (!r.ok) {
    console.log("polar", method, path, r.status, t.slice(0, 800));
    throw new Error(`polar ${r.status}`);
  }
  try { return JSON.parse(t); } catch { return {}; }
}

const planName = (gb) => (gb >= 1000 ? `${gb / 1000} TB` : `${gb} GB`);
let plansCache = null, plansAt = 0;
/// products for sale: [{id, name, gb, interval, price, currency}], small first, monthly before yearly
async function plans(env) {
  if (plansCache && now() - plansAt < 600) return plansCache;
  const j = await polar(env, "GET", "/v1/products/?is_recurring=true&is_archived=false&limit=100");
  plansCache = (j.items || []).map((p) => {
    const pr = (p.prices || []).find((x) => x.amount_type === "fixed" && !x.is_archived);
    const gb = Number(p.metadata && p.metadata.quota_gb);
    if (!pr || !gb) return null;
    return { id: p.id, name: p.name, gb, interval: p.recurring_interval, price: pr.price_amount / 100, currency: pr.price_currency };
  }).filter(Boolean).sort((a, b) => a.gb - b.gb || (a.interval === "month" ? -1 : 1));
  plansAt = now();
  return plansCache;
}

async function portal(user, env) {
  const j = await polar(env, "POST", "/v1/customer-sessions/", { external_customer_id: user.id });
  return json({ url: j.customer_portal_url, portal: true });
}

async function checkout(req, user, env, origin) {
  const b = await body(req);
  const p = (await plans(env)).find((x) => x.id === b.product_id);
  if (!p) return fail("That plan isn't available");
  // already paying: switching plan or cancelling happens on Polar's subscription page
  if (user.polar_sub) return portal(user, env);
  const j = await polar(env, "POST", "/v1/checkouts/", {
    products: [p.id],
    external_customer_id: user.id,
    customer_email: user.email,
    metadata: { echo_user: user.id },
    success_url: `${origin}/thanks`,
  });
  return json({ url: j.url });
}

/// Standard Webhooks signature (Polar): HMAC-SHA256 of "<id>.<timestamp>.<body>"
async function polarVerify(req, env, raw) {
  const id = req.headers.get("webhook-id"), ts = req.headers.get("webhook-timestamp");
  const given = (req.headers.get("webhook-signature") || "").split(" ").map((x) => x.split(",")[1]).filter(Boolean);
  const secret = String(env.POLAR_WEBHOOK_SECRET || "").trim();
  if (!id || !ts || !secret || !given.length || Math.abs(now() - Number(ts)) > 600) return false;
  const keys = [new TextEncoder().encode(secret)]; // older Polar secrets: the whole whsec_ string
  try { keys.unshift(Uint8Array.from(atob(secret.replace(/^whsec_/, "")), (c) => c.charCodeAt(0))); } catch { /* not base64 */ }
  const msg = new TextEncoder().encode(`${id}.${ts}.${raw}`);
  for (const k of keys) {
    const key = await crypto.subtle.importKey("raw", k, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
    const sig = btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.sign("HMAC", key, msg))));
    if (given.includes(sig)) return true;
  }
  return false;
}

async function polarWebhook(req, env) {
  const raw = await req.text();
  if (!(await polarVerify(req, env, raw))) return fail("bad signature", 403);
  let ev;
  try { ev = JSON.parse(raw); } catch { return fail("bad json"); }
  const type = String(ev.type || ""), s = ev.data || {};
  if (!type.startsWith("subscription.")) return json({ ok: true }, 202);
  const uid = (s.customer && s.customer.external_id) || (s.metadata && s.metadata.echo_user);
  const user = uid && (await env.DB.prepare("SELECT * FROM users WHERE id = ?").bind(uid).first());
  if (!user) { console.log("polar: no Echo user for", type, s.id, uid); return json({ ok: true }, 202); }
  // a late retry of an older event must not undo a newer one (e.g. "active" arriving after "revoked")
  const at = Date.parse(s.modified_at || ev.timestamp || "") || 0;
  if (at && at < (user.polar_at || 0)) { console.log("polar: old event skipped", type, s.id); return json({ ok: true }, 202); }
  if (at) await env.DB.prepare("UPDATE users SET polar_at = ? WHERE id = ?").bind(at, uid).run();
  let gb = Number(s.product && s.product.metadata && s.product.metadata.quota_gb);
  if (!gb && s.product_id) {
    try { const p = await polar(env, "GET", `/v1/products/${s.product_id}`); gb = Number(p.metadata && p.metadata.quota_gb); } catch { /* logged */ }
  }
  // active until the paid period ends (also after cancelling); "revoked" = access is over
  const live = type !== "subscription.revoked" && ["active", "trialing", "past_due"].includes(s.status);
  if (live && gb) {
    const until = s.current_period_end ? Math.floor(Date.parse(s.current_period_end) / 1000) || 0 : 0;
    // real monthly amount after discounts (stats page): a 100% code like FREETEST = 0; "once" discounts only
    // lower the first payment, so they count at full price
    const cur = String(s.currency || "eur").toLowerCase();
    let cents = Number(s.amount) || 0;
    const dc = s.discount;
    if (dc && dc.duration !== "once") {
      if (dc.type === "percentage") cents = Math.round(cents * (1 - (Number(dc.basis_points) || 0) / 10000));
      else if (dc.type === "fixed") cents = Math.max(0, cents - (Number((dc.amounts && dc.amounts[cur]) ?? dc.amount) || 0));
    }
    if (s.recurring_interval === "year") cents = Math.round(cents / 12);
    await env.DB.prepare("UPDATE users SET plan = ?, quota_bytes = ?, polar_sub = ?, plan_until = ?, over_since = 0, over_stage = 0, over_removed = 0, sub_cents = ?, sub_currency = ? WHERE id = ?")
      .bind(planName(gb), gb * GB, s.id, until, cents, cur.toUpperCase(), uid).run();
    console.log("polar:", type, uid, "->", planName(gb));
  } else if (!live && (!user.polar_sub || user.polar_sub === s.id)) {
    await env.DB.prepare("UPDATE users SET plan = 'free', quota_bytes = ?, polar_sub = NULL, plan_until = 0, sub_cents = -1, sub_currency = NULL WHERE id = ?")
      .bind(Number(env.FREE_GB || 10) * GB, uid).run();
    console.log("polar:", type, uid, "-> free");
  }
  return json({ ok: true }, 202);
}

const page = (title, text) => new Response(
  `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${title}</title>`
  + `<body style="margin:0;min-height:100vh;display:grid;place-items:center;background:#0e0f13;color:#e8e8ee;font-family:system-ui,Segoe UI,Arial,sans-serif">`
  + `<div style="max-width:420px;padding:24px;text-align:center"><h1 style="font-size:26px">${title}</h1><p style="color:#a0a3ad;line-height:1.5">${text}</p></div>`,
  { headers: { "content-type": "text/html; charset=utf-8" } });

// ----- downloads: the GitHub repo is private, so /download/<what> redirects to a short-lived signed GitHub link -----
// Secret GITHUB_TOKEN = fine-grained token, only repo Resister0/echo, permission "Contents: Read-only".
const DOWNLOADS = {
  "windows": "Echo-Setup.exe",
  "windows-portable": "Echo.exe",
  "linux": "Echo-x86_64.AppImage",
  "linux-tar": "Echo-linux-x86_64.tar.gz",
};
// Releases are built in two places: the PUBLIC repo Resister0/echoclip (traceable builds, needed for code
// signing later; its files download without a token) and, as a fallback, the private repo Resister0/echo
// (needs GITHUB_TOKEN). Both publish a release "latest" titled "Echo 1.0.<n>" with the same number;
// for each file the newest release that has it wins, the public one on a tie.
const releaseCache = new Map();   // repo -> { at, rel }
const relSaved = new Map();       // repo -> when this Worker copy last saved the release to D1 (table lastrel)
const ghFetch = (env, url, accept, auth = true) => fetch(url, {
  headers: { ...(auth && env.GITHUB_TOKEN ? { authorization: `Bearer ${env.GITHUB_TOKEN}` } : {}), accept, "user-agent": "echo-cloud", "x-github-api-version": "2022-11-28" },
  redirect: "manual",
});
const publicRepo = (env) => env.PUBLIC_REPO || "Resister0/echoclip";
const privateRepo = (env) => env.GITHUB_REPO || "Resister0/echo";
/// the GitHub release "latest" of one repo (cached 2 min), or null
async function releaseOf(env, repo, isPublic) {
  // the public build tells us about each release itself (POST /release/push) - no GitHub lookup needed
  // (chat 22: some Cloudflare locations couldn't read GitHub and kept offering 1.0.37)
  if (isPublic) { const p = await pushedRelease(env); if (p) return p; }
  const c = releaseCache.get(repo);
  // 2 min: right after a release, Cloudflare locations kept saying "newest version" for up to 5 min (chat 18)
  if (c && now() - c.at < (c.rel ? 120 : 15)) return c.rel;
  const url = `https://api.github.com/repos/${repo}/releases/tags/latest`;
  let r = null;
  try {
    r = isPublic || env.GITHUB_TOKEN ? await ghFetch(env, url, "application/vnd.github+json") : null;
    // the token may only be allowed for the private repo: public releases can be read without it
    if (r && !r.ok && isPublic && env.GITHUB_TOKEN) r = await ghFetch(env, url, "application/vnd.github+json", false);
  } catch (e) { console.log("github: release lookup error", repo, e && e.message); r = { ok: false, status: 0, text: async () => "" }; }
  // last good answer, shared by all Worker copies in this Cloudflare location (survives a GitHub hiccup /
  // rate limit, which used to make /update say "up to date" now and then - chat 20)
  const ck = new Request(`https://echoclip.link/_cache/release/${repo}`);
  let rel = null;
  if (r && r.ok) {
    rel = await r.json();
    rel.public = isPublic;
    try { await caches.default.put(ck, new Response(JSON.stringify(rel), { headers: { "content-type": "application/json", "cache-control": "max-age=604800" } })); } catch {}
    // and in D1 (one copy for every Cloudflare location), at most every 10 min per Worker copy
    if (now() - (relSaved.get(repo) || 0) > 600 && env.DB) {
      relSaved.set(repo, now());
      try {
        await env.DB.prepare("CREATE TABLE IF NOT EXISTS lastrel (repo TEXT PRIMARY KEY, rel TEXT NOT NULL, at INTEGER NOT NULL)").run();
        const slim = { name: rel.name, body: rel.body, published_at: rel.published_at, created_at: rel.created_at, public: rel.public,
          assets: (rel.assets || []).map((a) => ({ id: a.id, name: a.name, url: a.url, browser_download_url: a.browser_download_url })) };
        await env.DB.prepare("INSERT OR REPLACE INTO lastrel (repo, rel, at) VALUES (?, ?, ?)").bind(repo, JSON.stringify(slim), now()).run();
      } catch (e) { console.log("github: saving last release failed", e && e.message); }
    }
  } else {
    if (r && r.status !== 404) console.log("github: release lookup failed", repo, r.status, (await r.text()).slice(0, 200));
    // 404 = the release is being rebuilt right now (its files are gone too): nothing to offer, like before.
    // Any other failure (rate limit, bad token): public releases can be read from github.com itself (no API
    // limit - chat 21: the API kept failing and /update offered the stale 1.0.37 while 1.0.43 was out),
    // else the last good release
    if (r && r.status !== 404 && isPublic) rel = await releaseFromWeb(repo).catch((e) => { console.log("github: web lookup error", e && e.message); return null; });
    const fromWeb = !!rel;
    if (r && r.status !== 404 && !rel) rel = (c && c.rel) || null;
    if (r && r.status !== 404 && !rel) try { const hit = await caches.default.match(ck); if (hit) rel = await hit.json(); } catch {}
    if (r && r.status !== 404 && !rel && env.DB) try {
      const row = await env.DB.prepare("SELECT rel FROM lastrel WHERE repo = ?").bind(repo).first();
      if (row) rel = JSON.parse(row.rel);
    } catch {}
    if (rel && !fromWeb) console.log("github: using the last known release of", repo, relVersion(rel));
  }
  releaseCache.set(repo, { at: now(), rel });
  return rel;
}
/// the public release "latest" without the GitHub API: title + notes from the releases feed, files checked
/// with HEAD on their github.com download links (302 = there). Same shape as the API answer.
async function releaseFromWeb(repo) {
  const feed = await fetch(`https://github.com/${repo}/releases.atom`, { headers: { "user-agent": "echo-cloud", accept: "application/atom+xml" } });
  if (!feed.ok) { console.log("github: releases feed failed", repo, feed.status); return null; }
  const xml = await feed.text();
  const entry = (xml.match(/<entry>([\s\S]*?)<\/entry>/) || [])[1] || "";
  const unesc = (t) => t.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, "&");
  const name = unesc(((entry.match(/<title[^>]*>([\s\S]*?)<\/title>/) || [])[1] || "").trim());
  const tag = (((entry.match(/<link[^>]*href="([^"]+)"/) || [])[1] || "").match(/\/releases\/tag\/([^"/?#]+)/) || [])[1];
  if (!/\d+\.\d+\.\d+/.test(name) || tag !== "latest") { console.log("github: releases feed has no usable entry", repo, name, tag); return null; }
  const html = unesc((entry.match(/<content[^>]*>([\s\S]*?)<\/content>/) || [])[1] || "");
  const body = unesc(html.replace(/<br\s*\/?>/gi, "\n").replace(/<\/(p|li|div|h\d)>/gi, "\n").replace(/<[^>]+>/g, ""));
  const updated = ((entry.match(/<updated>([^<]+)<\/updated>/) || [])[1] || "").trim();
  const v = (name.match(/\d+\.\d+\.\d+/) || [])[0];
  const names = [];
  for (const f of Object.values(DOWNLOADS)) names.push(f, `${f}.sig`);
  const have = await Promise.all(names.map(async (n) => {
    try {
      const h = await fetch(`https://github.com/${repo}/releases/download/latest/${encodeURIComponent(n)}`, { method: "HEAD", redirect: "manual", headers: { "user-agent": "echo-cloud" } });
      return h.status >= 300 && h.status < 400;
    } catch { return false; }
  }));
  const assets = names.filter((n, i) => have[i]).map((n) => ({ id: `web:${v}:${n}`, name: n,
    browser_download_url: `https://github.com/${repo}/releases/download/latest/${encodeURIComponent(n)}` }));
  if (!assets.length) { console.log("github: no release files found on the web for", repo, v); return null; }
  console.log("github: API failed, read", repo, v, "from github.com (", assets.length, "files)");
  return { name, body, published_at: updated, created_at: updated, public: true, assets };
}
const relVersion = (rel) => rel && ((rel.name || "").match(/\d+\.\d+\.\d+/) || [])[0];
/// the release the public build pushed last (D1 table pushedrel, cached 30 s per Worker copy), or null.
/// While a new build is replacing "latest" its files are briefly gone - the push only arrives once they're up.
let pushedCache = { at: 0, rel: null };
async function pushedRelease(env) {
  if (now() - pushedCache.at < 30) return pushedCache.rel;
  let rel = null;
  try {
    const row = await env.DB.prepare("SELECT rel FROM pushedrel WHERE id = 1").first();
    if (row) rel = JSON.parse(row.rel);
  } catch { /* table not there yet = never pushed */ }
  pushedCache = { at: now(), rel };
  return rel;
}
/// POST /release/push from the public repo's release job, header x-release-key = secret RELEASE_KEY
/// (also a GitHub secret in Resister0/echoclip). Body {version, notes, files: [names], sigs: {name: text}}.
async function releasePush(req, env) {
  const key = req.headers.get("x-release-key") || "", want = env.RELEASE_KEY || "";
  if (!(await allow(env, `relpush:${ipOf(req)}`, 30, 3600))) return fail("slow down", 429);
  let diff = want.length < 20 || key.length !== want.length ? 1 : 0;
  for (let i = 0; i < want.length; i++) diff |= want.charCodeAt(i) ^ (key.charCodeAt(i) || 0);
  if (diff) return fail("Wrong release key", 403);
  const b = await req.json().catch(() => null);
  const v = b && String(b.version || "");
  if (!/^\d+\.\d+\.\d+$/.test(v) || !Array.isArray(b.files)) return fail("bad release", 400);
  const repo = publicRepo(env), sigs = b.sigs || {};
  const notes = String(b.notes || "").split("\n")[0].trim().slice(0, 600);
  const assets = b.files.map(String).filter((n) => /^[A-Za-z0-9._-]{1,80}$/.test(n)).map((n) => ({
    id: `push:${v}:${n}`, name: n, sig_text: typeof sigs[n] === "string" ? sigs[n].trim().slice(0, 4000) : undefined,
    browser_download_url: `https://github.com/${repo}/releases/download/latest/${encodeURIComponent(n)}` }));
  const at = now();
  const rel = { name: `Echo ${v}`, body: `Built from: ${notes}`, published_at: new Date(at * 1000).toISOString(),
    created_at: new Date(at * 1000).toISOString(), public: true, pushed: true, assets };
  await env.DB.prepare("CREATE TABLE IF NOT EXISTS pushedrel (id INTEGER PRIMARY KEY, rel TEXT NOT NULL, at INTEGER NOT NULL)").run();
  await env.DB.prepare("INSERT OR REPLACE INTO pushedrel (id, rel, at) VALUES (1, ?, ?)").bind(JSON.stringify(rel), at).run();
  try {
    await env.DB.prepare("CREATE TABLE IF NOT EXISTS releases (version TEXT PRIMARY KEY, notes TEXT NOT NULL, at INTEGER NOT NULL, sha TEXT)").run();
    if (notes) await env.DB.prepare("INSERT OR IGNORE INTO releases (version, notes, at, sha) VALUES (?, ?, ?, NULL)").bind(v, notes, at).run();
  } catch {}
  pushedCache = { at: now(), rel };
  console.log("release: pushed", v, assets.length, "files");
  return json({ ok: true, version: v, files: assets.length });
}
/// newest release that has `file` (and its .sig when `withSig`); "private" = only the private repo
async function release(env, file, withSig, only) {
  const rels = [];
  if (only !== "private") rels.push(await releaseOf(env, publicRepo(env), true));
  rels.push(await releaseOf(env, privateRepo(env), false));
  const has = (rel, n) => (rel.assets || []).some((a) => a.name === n);
  let best = null;
  for (const rel of rels) {
    if (!rel || !relVersion(rel) || !has(rel, file) || (withSig && !has(rel, `${file}.sig`))) continue;
    if (!best || newer(relVersion(rel), relVersion(best))) best = rel;
  }
  return best;
}
/// download address of a release file: public files directly, private ones as a short-lived signed address
async function assetLink(env, rel, asset) {
  if (rel.public) return asset.browser_download_url;
  const r = await ghFetch(env, asset.url, "application/octet-stream");
  const to = r.headers.get("location");
  if (!to) console.log("github: no redirect for", asset.name, r.status);
  return to;
}

async function download(env, what, from) {
  // Linux Flatpak: the one-click install file of Echo's own Flatpak repo (ci/flatpak-repo.sh in the public repo
  // writes it to the R2 bucket echo-flatpak, served on flatpak.echoclip.link). Discover / GNOME Software open it.
  if (what === "flatpak") {
    return new Response(null, { status: 302, headers: { location: env.FLATPAK_REF || "https://flatpak.echoclip.link/echo.flatpakref", "cache-control": "no-store" } });
  }
  const file = DOWNLOADS[what];
  if (!file) return null;
  const rel = await release(env, file, false, from);
  if (!rel) {
    releaseCache.clear();
    return page("Download not available", "This build isn't ready right now - a new version may be building. Try again in about 10 minutes.");
  }
  const to = await assetLink(env, rel, rel.assets.find((a) => a.name === file));
  if (!to) return page("Download not available", "Try again in a few minutes.");
  return new Response(null, { status: 302, headers: { location: to, "cache-control": "no-store" } });
}

// ----- updates for Echo (Tauri updater): GET /update/<windows|linux>/<arch>/<current version> -----
// 204 = up to date; otherwise {version, notes, pub_date, url, signature}. The version comes from the
// release title "Echo 1.0.<n>" (ci/build.yml), the signature from the <file>.sig next to the installer.
const UPDATE_FILES = { windows: "windows", linux: "linux" };
const verNum = (v) => String(v || "").split(/[.+-]/).slice(0, 3).map((x) => parseInt(x, 10) || 0);
function newer(a, b) {
  const x = verNum(a), y = verNum(b);
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] > y[i];
  return false;
}
const sigCache = new Map();
async function update(env, target, current) {
  const what = UPDATE_FILES[target];
  const none = () => new Response(null, { status: 204 });
  if (!what) return none();
  const file = DOWNLOADS[what];
  const rel = await release(env, file, true);   // still building / unsigned build -> not offered
  const version = relVersion(rel);
  if (!version || !newer(version, current)) return none();
  const sig = rel.assets.find((a) => a.name === `${file}.sig`);
  let signature = sig.sig_text || sigCache.get(sig.id);
  if (!signature) {
    const to = await assetLink(env, rel, sig);
    const r = to && (await fetch(to));
    if (!r || !r.ok) return none();
    signature = (await r.text()).trim();
    sigCache.set(sig.id, signature);
  }
  const notes = ((rel.body || "").match(/Built from: (.*)/) || [])[1] || "";
  // the installer from the SAME release as the signature
  const url = rel.public ? rel.assets.find((a) => a.name === file).browser_download_url : `https://echoclip.link/download/${what}?from=private`;
  return json({ version, notes, pub_date: rel.published_at || rel.created_at, url, signature });
}

// ----- changelog: every published version + its "What changed?" text -----
// Each publish is one commit in the public repo that changes ci/version.txt; its message is William's
// "What changed?" text. Read from GitHub (no extra keys), kept in D1 table releases. Used by Echo's "What's new"
// card (earlier updates you skipped) and echoclip.link/changelog.
let changesSynced = 0;
async function syncReleases(env) {
  await env.DB.prepare("CREATE TABLE IF NOT EXISTS releases (version TEXT PRIMARY KEY, notes TEXT NOT NULL, at INTEGER NOT NULL, sha TEXT)").run();
  const repo = publicRepo(env);
  const url = `https://api.github.com/repos/${repo}/commits?path=ci/version.txt&per_page=25`;
  let r = await ghFetch(env, url, "application/vnd.github+json");
  if (!r.ok && env.GITHUB_TOKEN) r = await ghFetch(env, url, "application/vnd.github+json", false);
  if (!r.ok) { console.log("changes: commit list failed", r.status); return; }
  const commits = await r.json();
  const known = new Set((((await env.DB.prepare("SELECT sha FROM releases").all()).results) || []).map((x) => x.sha));
  for (const c of commits) {
    if (!c || !c.sha || known.has(c.sha)) continue;
    const raw = await fetch(`https://raw.githubusercontent.com/${repo}/${c.sha}/ci/version.txt`);
    const v = raw.ok ? (await raw.text()).trim() : "";
    if (!/^\d+\.\d+\.\d+$/.test(v)) continue;
    const notes = String((c.commit && c.commit.message) || "").split("\n")[0].trim().slice(0, 600);
    const at = Math.floor(Date.parse((c.commit && c.commit.committer && c.commit.committer.date) || "") / 1000) || now();
    await env.DB.prepare("INSERT OR REPLACE INTO releases (version, notes, at, sha) VALUES (?, ?, ?, ?)").bind(v, notes, at, c.sha).run();
  }
}
async function changes(env, since) {
  if (now() - changesSynced > 120) {
    changesSynced = now();
    try { await syncReleases(env); } catch (e) { console.log("changes: sync failed", e && e.message); }
  }
  let rows = [];
  try { rows = ((await env.DB.prepare("SELECT version, notes, at FROM releases ORDER BY at DESC LIMIT 200").all()).results) || []; } catch { /* table not there yet */ }
  // only versions that really came out (a build can fail after its commit)
  const latestRel = await releaseOf(env, publicRepo(env), true).catch(() => null);
  const latest = relVersion(latestRel);
  // the commit list (GitHub API) can fail; the newest release's own notes keep the changelog current
  if (latest && !rows.some((x) => x.version === latest)) {
    const notes = (((latestRel.body || "").match(/Built from: (.*)/) || [])[1] || "").trim().slice(0, 600);
    const at = Math.floor(Date.parse(latestRel.published_at || latestRel.created_at || "") / 1000) || now();
    if (notes) {
      rows.push({ version: latest, notes, at });
      try { await env.DB.prepare("INSERT OR IGNORE INTO releases (version, notes, at, sha) VALUES (?, ?, ?, NULL)").bind(latest, notes, at).run(); } catch {}
    }
  }
  // hidden from "What's new" / the changelog: an emptied text (D1: UPDATE releases SET notes='' WHERE version='…'),
  // or a "What changed?" text starting with "#" (internal builds, chat 26)
  let list = rows.filter((x) => x.notes && !String(x.notes).trim().startsWith("#") && (!latest || !newer(x.version, latest)));
  list.sort((a, b) => (newer(a.version, b.version) ? -1 : newer(b.version, a.version) ? 1 : 0));
  if (since) list = list.filter((x) => newer(x.version, since));
  return new Response(JSON.stringify({ latest: latest || null, releases: list.slice(0, 60) }), {
    headers: { "content-type": "application/json", "cache-control": "public, max-age=60" },
  });
}

async function notFound(req, env) {
  // browsers get the site's 404 page, Echo gets JSON
  if (req.method === "GET" && env.ASSETS && (req.headers.get("accept") || "").includes("text/html")) {
    const p = await env.ASSETS.fetch(new Request(new URL("/404.html", req.url)));
    if (p.ok) return new Response(p.body, { status: 404, headers: p.headers });
  }
  return fail("not found", 404);
}

function gone() {
  return new Response("This clip link has expired or the clip was removed.", { status: 404, headers: { "content-type": "text/plain; charset=utf-8" } });
}

async function serveShare(req, env, id, ext) {
  const row = await env.DB.prepare("SELECT * FROM shares WHERE id = ?").bind(id).first();
  if (!row || (row.expires && row.expires < now())) return gone();
  if (ext === ".jpg") {
    const t = row.thumb && (await env.BUCKET.get(`thumbs/${id}.jpg`));
    if (!t) return new Response("no thumbnail", { status: 404 });
    return new Response(req.method === "HEAD" ? null : t.body, { headers: { "content-type": "image/jpeg", "cache-control": "public, max-age=86400", "x-content-type-options": "nosniff" } });
  }
  // the bare link: a page for browsers and link previews (Discord, …); video players get the file as before
  if (!ext) {
    const ua = req.headers.get("user-agent") || "", acc = req.headers.get("accept") || "";
    if (BOTS.test(ua) || (acc.includes("text/html") && !req.headers.has("range"))) return sharePage(req, row, id, await profileOf(env, row.user_id));
  }
  const name = row.key.split("/").pop();
  const ascii = name.replace(/[^\x20-\x7e]/g, "_").replace(/["\\]/g, "");
  const h = new Headers({
    "content-type": "video/mp4",
    "accept-ranges": "bytes",
    "cache-control": "public, max-age=3600",
    "content-disposition": `inline; filename="${ascii}"; filename*=UTF-8''${encodeURIComponent(name)}`,
    "access-control-allow-origin": "*",
    // users upload whatever they like: never let a browser run it as a page on echoclip.link
    "x-content-type-options": "nosniff",
    // (only "sandbox": a stricter policy also blocks the browser's own video player)
    "content-security-policy": "sandbox",
  });
  if (req.method === "HEAD") {
    const o = await env.BUCKET.head(row.key);
    if (!o) return gone();
    h.set("content-length", String(o.size));
    h.set("etag", o.httpEtag);
    return new Response(null, { headers: h });
  }
  const wantsRange = req.headers.has("range");
  let o;
  try { o = await env.BUCKET.get(row.key, wantsRange ? { range: req.headers } : {}); }
  catch { o = await env.BUCKET.get(row.key); }
  if (!o) return gone();
  h.set("etag", o.httpEtag);
  const r = wantsRange && o.range;
  if (r) {
    let off, len;
    if ("suffix" in r) { len = Math.min(r.suffix, o.size); off = o.size - len; }
    else { off = r.offset || 0; len = r.length ?? o.size - off; }
    h.set("content-range", `bytes ${off}-${off + len - 1}/${o.size}`);
    h.set("content-length", String(len));
    return new Response(o.body, { status: 206, headers: h });
  }
  h.set("content-length", String(o.size));
  return new Response(o.body, { headers: h });
}

const AUTHED = new Set(["GET /profile/me", "POST /profile", "POST /profile/avatar", "POST /profile/pin", "POST /profile/delete", "POST /follow", "GET /me", "POST /creds", "POST /share", "POST /shares/views", "POST /checkout", "POST /portal", "POST /auth/logout",
  "POST /upload", "POST /delete", "POST /move", "POST /share/remove", "POST /account/delete"]);

export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    const route = `${req.method} ${url.pathname.replace(/\/+$/, "") || "/"}`;
    try {
      await migrate(env);
      const sm = url.pathname.match(/^\/s\/([a-z0-9]{6,20})(\.mp4|\.jpg)?\/?$/);
      if (sm && (req.method === "GET" || req.method === "HEAD")) return await serveShare(req, env, sm[1], sm[2] || "");
      // GET / and the other website pages come from site/ (static assets, answered before this code runs)
      if (route === "GET /health") return json({ service: "echo-cloud", ok: true });
      const dm = url.pathname.match(/^\/download\/([a-z-]+)\/?$/);
      if (dm && req.method === "GET") {
        const r = await download(env, dm[1], url.searchParams.get("from") === "private" ? "private" : "");
        if (r && r.status === 302) await countDownload(req, env, dm[1]);
        return r || (await notFound(req, env));
      }
      if (route === "POST /hit") return await hit(req, env);
      const vm = url.pathname.match(/^\/s\/([a-z0-9]{6,20})\/view\/?$/);
      if (vm && req.method === "POST") return await shareView(req, env, vm[1]);
      const pm = url.pathname.match(/^\/u\/([a-z0-9_]{3,20})(\/avatar\.jpg)?\/?$/);
      if (pm && (req.method === "GET" || req.method === "HEAD")) return pm[2] ? await avatarFile(env, pm[1]) : await profilePage(req, env, pm[1]);
      const om = url.pathname.match(/^\/oembed\/([a-z0-9]{6,20})\/?$/);
      if (om && req.method === "GET") return await shareOembed(req, env, om[1]);
      const um = url.pathname.match(/^\/update\/([a-z]+)\/([a-z0-9_]+)\/([0-9][0-9a-z.+-]{0,30})\/?$/);
      if (um && req.method === "GET") return await update(env, um[1], um[3]);
      if (route === "GET /thanks") return page("Thanks for upgrading!", "Your Echo Cloud space goes up within a minute. You can close this tab and go back to Echo.");
      if (route === "GET /changes") {
        const since = (url.searchParams.get("since") || "").match(/^\d+\.\d+\.\d+$/);
        return await changes(env, since ? since[0] : "");
      }
      if (route === "GET /plans") return json({ plans: await plans(env) });
      if (route === "POST /polar/webhook") return await polarWebhook(req, env);
      if (route === "POST /auth/start") return await authStart(req, env);
      if (route === "POST /auth/verify") return await authVerify(req, env);
      if (route === "POST /release/push") return await releasePush(req, env);
      if (route === "POST /admin/takedown") return await takedown(req, env);
      if (route === "POST /admin/profile-takedown") return await profileTakedown(req, env);
      if (route === "POST /admin/profile-handle") return await profileHandle(req, env);
      if (route === "GET /admin/stats") { const r = await adminStats(req, env); r.headers.set("cache-control", "no-store"); return r; }
      if (route === "POST /log") return await logUpload(req, env, url.origin);
      const lm = url.pathname.match(/^\/log\/([a-z2-9]{14})\/?$/);
      if (lm && req.method === "GET") return await logRead(req, env, lm[1]);
      if (route === "POST /feedback") return await feedback(req, env, await userFromToken(req, env).catch(() => null));
      const user = await userFromToken(req, env);
      if (!user) return AUTHED.has(route) || req.headers.has("authorization")
        ? fail("Signed out - sign in to Echo Cloud again", 401) : await notFound(req, env);
      if (route === "GET /me") return json(me(user, await usage(env, user, url.searchParams.has("fresh"))));
      if (route === "POST /creds") return await creds(user, env);
      if (route === "POST /share") return await share(req, user, env, url.origin);
      if (route === "POST /share/remove") return await unshare(req, user, env);
      if (route === "POST /shares/views") return await shareViews(req, user, env);
      if (route === "POST /upload") return await upload(req, user, env);
      if (route === "POST /delete") return await del(req, user, env);
      if (route === "POST /move") return await move(req, user, env);
      if (route === "POST /account/delete") return await deleteAccount(req, user, env);
      if (route === "GET /profile/me") return await profileMe(user, env, url.origin);
      if (route === "POST /profile") return await profileSave(req, user, env, url.origin);
      if (route === "POST /profile/avatar") return await profileAvatar(req, user, env, url.origin);
      if (route === "POST /profile/pin") return await profilePin(req, user, env);
      if (route === "POST /profile/delete") return await profileDelete(user, env);
      if (route === "POST /follow") return await followSet(req, user, env, url.origin);
      if (route === "POST /checkout") return await checkout(req, user, env, url.origin);
      if (route === "POST /portal") return await portal(user, env);
      if (route === "POST /auth/logout") {
        const t = (req.headers.get("authorization") || "").slice(7).trim();
        await env.DB.prepare("DELETE FROM sessions WHERE token_hash = ?").bind(await sha256(t)).run();
        return json({ ok: true });
      }
      return await notFound(req, env);
    } catch (e) {
      console.log("error", e && e.stack);
      return fail("Echo Cloud had a problem - try again", 500);
    }
  },
  async scheduled(event, env, ctx) {
    ctx.waitUntil(cleanupExpired(env).then(() => cleanupStrayCopies(env)).catch((e) => console.log("cleanup error", e && e.stack)));
    ctx.waitUntil(backupDb(env).catch((e) => console.log("backup error", e && e.stack)));
    ctx.waitUntil(syncReleases(env).catch((e) => console.log("changes sync error", e && e.stack)));
    ctx.waitUntil(cleanupLogs(env).catch((e) => console.log("logs cleanup error", e && e.stack)));
    ctx.waitUntil(cleanupStats(env).catch((e) => console.log("stats cleanup error", e && e.stack)));
    ctx.waitUntil(enforceQuota(env).catch((e) => console.log("quota error", e && e.stack)));
  },
};

