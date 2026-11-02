# Echo Cloud server

A small Cloudflare Worker: sign in with an email code, usage/quota, and short-lived R2 keys that only
reach the user's own folder (`u/<id>/` in the `echo-cloud` bucket). Echo stores clips straight in R2 with them.

## One-time setup (Cloudflare dashboard + a terminal in this folder)

1. **R2 bucket**: Cloudflare dashboard → R2 → Create bucket → name `echo-cloud` (private, no public access).
2. **R2 API token** (the "parent" key the temporary keys are made from): R2 → Manage API tokens → Create
   → *Object Read & Write*, only bucket `echo-cloud`. Note the **Access Key ID** (secret key is not needed).
3. **Cloudflare API token** (lets the Worker make temporary keys): My Profile → API Tokens → Create token
   → Custom → permission *Account · Workers R2 Storage · Edit*.
4. **Account ID**: right side of the R2 page.
5. In this folder:
   ```
   npm i -g wrangler
   wrangler login
   wrangler d1 create echo-cloud          # copy the database_id into wrangler.toml
   wrangler d1 execute echo-cloud --remote --file schema.sql
   wrangler secret put ACCOUNT_ID
   wrangler secret put PARENT_ACCESS_KEY_ID
   wrangler secret put CF_API_TOKEN
   wrangler deploy                        # prints https://echo-cloud.<you>.workers.dev
   ```
6. Put that address into `EC_API_DEFAULT` in `src-tauri/src/recorder.rs` and build Echo.
   (Until then the Echo Cloud box stays hidden.)

## Emails

Without `RESEND_KEY` the sign-in code is only printed in the log: `wrangler tail` (fine for testing).
For real users: make a Resend account, verify your domain there (DNS records), set `MAIL_FROM` in
wrangler.toml (e.g. `Echo <noreply@yourdomain>`), then `wrangler secret put RESEND_KEY` and deploy again.

## Plans

New users get `FREE_GB` (10 GB). Paid plans are the Polar products with metadata `quota_gb`; the Polar
webhook (`/polar/webhook`) sets `plan` / `quota_bytes` in the `users` table.
Over the quota Echo only gets read-only keys (nothing is ever deleted).

## Website (site/)

Plain HTML/CSS in `site/` = https://echoclip.link (home, /pricing, /terms, /privacy, 404 page).
`wrangler deploy` uploads it together with the server. Files win over the Worker for their own paths,
so never add a file or folder named like an API route (s, plans, auth, polar, download, thanks, me, creds, share).
Download buttons use `/download/windows|windows-portable|linux|linux-tar`: the Worker redirects to the
newest GitHub release file. Needs the secret `GITHUB_TOKEN` (fine-grained, only this repo, Contents: Read-only).

## API

| Call | Body / header | Answer |
|---|---|---|
| `POST /auth/start` | `{email}` | emails a 6-digit code (10 min, 5 tries) |
| `POST /auth/verify` | `{email, code}` | `{token, id, email, plan, used, quota}` |
| `GET /me` | `Authorization: Bearer <token>` | `{id, email, plan, used, quota}` |
| `POST /creds` | token | `{endpoint, bucket, root, access_key_id, secret_access_key, session_token, expires, read_only, …}` |
| `POST /auth/logout` | token | `{ok}` |
| `GET /health` | – | `{service, ok}` |
