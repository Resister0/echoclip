#!/usr/bin/env python3
"""Wraps the page bodies in pages/*.html with the shared header + footer -> site/*.html"""
import pathlib, re

ROOT = pathlib.Path(__file__).parent
LOGO = ('<svg viewBox="0 0 64 64" aria-hidden="true"><g opacity=".72"><path d="M11.8 40 A21.75 21.75 0 1 1 35.6 53.5 A1.75 1.75 0 0 1 35 50 A18.25 18.25 0 1 0 15 38.7 A1.75 1.75 0 0 1 11.8 40Z" fill="#fff" stroke-width="0"/><path d="M18 16.7 L9.4 21.2 L17.7 26.4Z" fill="#fff" stroke="#fff" stroke-width="2" stroke-linejoin="round"/></g><path d="M27.6 21.4 L43.2 31.9 L27.6 42.4Z" fill="#fff" stroke="#fff" stroke-width="2" stroke-linejoin="round"/></svg>')

HEAD = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<meta name="description" content="{desc}">
<meta name="theme-color" content="#7c5cff">
<meta property="og:title" content="{title}">
<meta property="og:description" content="{desc}">
<meta property="og:site_name" content="Echo">
<meta property="og:type" content="website">
<meta property="og:image" content="https://echoclip.link/og.png?v=2">
<meta name="twitter:card" content="summary_large_image">
<link rel="icon" href="/favicon.svg?v=2" type="image/svg+xml">
<link rel="stylesheet" href="/style.css">
<script>try{{var t=localStorage.getItem("echo-theme");if(t)document.documentElement.dataset.theme=t}}catch(e){{}}</script>
</head>
<body>
<header><div class="wrap">
  <a class="brand" href="/"><span class="logo">{logo}</span>Echo</a>
  <nav>
    <a href="/#features" class="hide-s">Features</a>
    <a href="/#storage">Storage</a>
    <a href="https://discord.gg/JPVewEfUQs" class="hide-s">Discord</a>
    <button class="themebtn" id="themebtn" type="button" title="Theme" aria-label="Theme" aria-expanded="false"><svg viewBox="0 0 24 24"><path d="M12 3a9 9 0 1 0 0 18c1.1 0 1.6-.8 1.6-1.6 0-.5-.2-.8-.4-1.1-.3-.3-.4-.7-.4-1.1 0-.9.7-1.6 1.6-1.6H16a5 5 0 0 0 5-5C21 6.6 17 3 12 3z"/><circle cx="7.5" cy="11" r="1"/><circle cx="10" cy="7" r="1"/><circle cx="14.5" cy="7" r="1"/><circle cx="17" cy="11" r="1"/></svg></button>
    <a href="/download" class="btn primary small">Download</a>
  </nav>
  <div class="themes" id="themes" hidden><p>Theme</p></div>
</div></header>
<main>
"""

FOOT = """</main>
<footer><div class="wrap">
  <span>© 2026 Echo · Made in Denmark · <a href="https://github.com/Resister0/echoclip">Open source (GPL-3.0)</a></span>
  <nav>
    <a href="/pricing">Pricing</a>
    <a href="/changelog">Changelog</a>
    <a href="https://discord.gg/JPVewEfUQs">Discord</a>
    <a href="/terms">Terms</a>
    <a href="/privacy">Privacy</a>
    <a href="/code-signing">Code signing</a>
    <a href="mailto:support@echoclip.link">support@echoclip.link</a>
  </nav>
</div></footer>
<script src="/site.js"></script>
</body>
</html>
"""

out = ROOT.parent / "site"   # this folder is cloud/site-src
for src in sorted((ROOT / "pages").glob("*.html")):
    text = src.read_text(encoding="utf-8")
    m = re.match(r"<!--\s*title:\s*(.*?)\s*\|\s*desc:\s*(.*?)\s*-->\n", text)
    assert m, f"{src.name}: first line must be <!-- title: ... | desc: ... -->"
    body = text[m.end():]
    body = body.replace('<span class="logo sm">LOGO</span>', f'<span class="logo sm">{LOGO}</span>')
    html = HEAD.format(title=m[1], desc=m[2], logo=LOGO,
                       on_pricing=' class="on"' if src.stem == "pricing" else "") + body + FOOT
    (out / src.name).write_text(html, encoding="utf-8")
    print("wrote", src.name, len(html))

# favicon = the app logo on the purple gradient
(out / "favicon.svg").write_text(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">'
    '<stop offset="0" stop-color="#8b6cff"/><stop offset=".55" stop-color="#6a4cf5"/><stop offset="1" stop-color="#4f7bff"/></linearGradient></defs>'
    '<rect width="256" height="256" rx="64" fill="url(#g)"/>' + LOGO.replace('<svg viewBox="0 0 256 256" aria-hidden="true">', '<g>').replace('</svg>', '</g>') + '</svg>',
    encoding="utf-8")

# canonical/Open Graph/JSON-LD/sitemap: the HEAD above has none of that, seo.py adds it back to site/*.html
import seo
seo.main()
