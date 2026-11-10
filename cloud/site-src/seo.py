#!/usr/bin/env python3
"""SEO upkeep for echoclip.link. Run after changing a page's <title>, description or the homepage FAQ:

    python3 cloud/site-src/seo.py          # update site/*.html heads + site/sitemap.xml, print a short report
    python3 cloud/site-src/seo.py --check  # change nothing; exit 1 if something is out of date

What it does (it only ever touches the <head> and sitemap.xml, never the visible page text):
- reads each page's <title> and <meta name="description"> (edit those by hand, they are the source of truth)
- writes one managed block per page between <!-- seo --> and <!-- /seo -->: canonical URL, Open Graph + Twitter
  tags (copied from the title/description), icons + manifest, and JSON-LD structured data
  (homepage: WebSite + Organization + SoftwareApplication + FAQPage built from the <details> FAQ; download page:
  SoftwareApplication). 404.html gets noindex instead.
- writes sitemap.xml (lastmod = the page's last git commit date)
- mirrors title/description into the first line of site-src/pages/*.html, so build_site.py keeps them

JSON-LD is a data block, not a script, so the CSP in site/_headers (script-src 'self' + one hash) doesn't affect it.
"""
import datetime, html, json, pathlib, re, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parent          # cloud/site-src
SITE = ROOT.parent / "site"
ORIGIN = "https://echoclip.link"
OG_IMAGE = f"{ORIGIN}/og.png?v=2"   # Discord caches images per URL: bump v when og.png changes
OG_ALT = "Echo - lightweight game clipping for any PC. One key saves the last moments. Free, no ads, no account."

# page file -> public path (the Worker serves download.html at /download etc.). Order = sitemap order.
PAGES = {
    "index.html": "/",
    "download.html": "/download",
    "pricing.html": "/pricing",
    "changelog.html": "/changelog",
    "code-signing.html": "/code-signing",
    "privacy.html": "/privacy",
    "terms.html": "/terms",
}
NOINDEX = ["404.html"]
PRIORITY = {"/": "1.0", "/download": "0.9", "/pricing": "0.7", "/changelog": "0.6"}

BEGIN, END = "<!-- seo: managed by cloud/site-src/seo.py, edit there -->", "<!-- /seo -->"
# loose tags from before seo.py existed (and anything it manages): removed from the head before the block goes in
LOOSE = re.compile(r'^\s*(<meta (property="og:|name="twitter:|name="robots")[^>]*>|<link rel="(canonical|icon|apple-touch-icon|manifest)"[^>]*>)\s*\n', re.M)


def text(s):
    """HTML fragment -> plain text"""
    return re.sub(r"\s+", " ", html.unescape(re.sub(r"<[^>]+>", "", s))).strip()


def head_info(page):
    t = re.search(r"<title>(.*?)</title>", page, re.S)
    d = re.search(r'<meta name="description" content="([^"]*)">', page)
    assert t and d, "page needs <title> and <meta name=\"description\" content=\"...\">"
    return t[1].strip(), d[1].strip()


def faq(page):
    qa = re.findall(r"<details><summary>(.*?)</summary>(.*?)</details>", page, re.S)
    return [{"@type": "Question", "name": text(q), "acceptedAnswer": {"@type": "Answer", "text": text(a)}} for q, a in qa]


def app(desc):
    return {
        "@type": "SoftwareApplication", "@id": f"{ORIGIN}/#app",
        "name": "Echo", "alternateName": "EchoClip",
        "applicationCategory": "MultimediaApplication", "applicationSubCategory": "Game clip recorder",
        "operatingSystem": "Windows 10, Windows 11, Linux",
        "description": desc,
        "url": f"{ORIGIN}/", "downloadUrl": f"{ORIGIN}/download",
        "image": OG_IMAGE, "screenshot": f"{ORIGIN}/screenshots/library.png",
        "license": "https://www.gnu.org/licenses/gpl-3.0.html",
        "isAccessibleForFree": True,
        "offers": {"@type": "Offer", "price": "0", "priceCurrency": "EUR"},
        "publisher": {"@id": f"{ORIGIN}/#org"},
    }


def jsonld(name, page, desc):
    org = {"@type": "Organization", "@id": f"{ORIGIN}/#org", "name": "Echo", "url": f"{ORIGIN}/",
           "logo": f"{ORIGIN}/icon-512.png", "email": "support@echoclip.link",
           "sameAs": ["https://github.com/Resister0/echoclip", "https://discord.gg/JPVewEfUQs"]}
    home = head_info(page if name == "index.html" else (SITE / "index.html").read_text(encoding="utf-8"))[1]
    if name == "index.html":
        graph = [{"@type": "WebSite", "@id": f"{ORIGIN}/#site", "name": "Echo", "url": f"{ORIGIN}/",
                  "publisher": {"@id": f"{ORIGIN}/#org"}}, org, app(text(home))]
        q = faq(page)
        if q:
            graph.append({"@type": "FAQPage", "@id": f"{ORIGIN}/#faq", "mainEntity": q})
    elif name == "download.html":
        graph = [org, app(text(home))]
    else:
        return ""
    data = json.dumps({"@context": "https://schema.org", "@graph": graph}, ensure_ascii=False, separators=(",", ":"))
    return '<script type="application/ld+json">' + data.replace("</", "<\\/") + "</script>\n"


def block(name, page):
    title, desc = head_info(page)
    lines = [BEGIN]
    if name in NOINDEX:
        lines.append('<meta name="robots" content="noindex">')
    else:
        lines.append(f'<link rel="canonical" href="{ORIGIN}{PAGES[name]}">')
        lines.append(f'<meta property="og:url" content="{ORIGIN}{PAGES[name]}">')
    lines += [
        f'<meta property="og:title" content="{title}">',
        f'<meta property="og:description" content="{desc}">',
        '<meta property="og:site_name" content="Echo">',
        '<meta property="og:type" content="website">',
        '<meta property="og:locale" content="en_US">',
        f'<meta property="og:image" content="{OG_IMAGE}">',
        '<meta property="og:image:width" content="1200">',
        '<meta property="og:image:height" content="630">',
        f'<meta property="og:image:alt" content="{html.escape(OG_ALT)}">',
        '<meta name="twitter:card" content="summary_large_image">',
        '<link rel="icon" href="/favicon.ico" sizes="48x48">',
        '<link rel="icon" href="/favicon.svg" type="image/svg+xml">',
        '<link rel="apple-touch-icon" href="/apple-touch-icon.png">',
        '<link rel="manifest" href="/site.webmanifest">',
    ]
    ld = "" if name in NOINDEX else jsonld(name, page, desc)
    return "\n".join(lines) + "\n" + ld + END + "\n"


def update(name, page):
    h_end = page.index("</head>")
    head, rest = page[:h_end], page[h_end:]
    head = re.sub(re.escape(BEGIN) + r".*?" + re.escape(END) + r"\n", "", head, flags=re.S)
    head = LOOSE.sub("", head)
    new = block(name, page)
    at = head.find('<link rel="stylesheet"')
    head = head[:at] + new + head[at:] if at >= 0 else head + new
    return head + rest


def lastmod(path):
    try:
        git = lambda *a: subprocess.run(["git", *a, "--", str(path)], cwd=ROOT, capture_output=True, text=True).stdout.strip()
        d = "" if git("status", "--porcelain") else git("log", "-1", "--format=%cs")   # edited, not committed yet = today
    except OSError:
        d = ""
    return d or datetime.date.today().isoformat()


def sitemap():
    rows = [f"  <url><loc>{ORIGIN}{p}</loc><lastmod>{lastmod(SITE / f)}</lastmod><priority>{PRIORITY.get(p, '0.4')}</priority></url>"
            for f, p in PAGES.items()]
    return '<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n' + "\n".join(rows) + "\n</urlset>\n"


def mirror_src(name, title, desc):
    src = ROOT / "pages" / name
    if not src.exists():
        return None
    t = src.read_text(encoding="utf-8")
    new = re.sub(r"\A<!--\s*title:.*?-->\n", f"<!-- title: {title} | desc: {desc} -->\n", t, count=1, flags=re.S)
    return (src, t, new)


def main(check=False):
    changed, report, titles = [], [], {}
    for name in list(PAGES) + NOINDEX:
        f = SITE / name
        page = f.read_text(encoding="utf-8")
        new = update(name, page)
        title, desc = head_info(new)
        if new != page:
            changed.append(f)
            if not check:
                f.write_text(new, encoding="utf-8")
        m = mirror_src(name, title, desc)
        if m and m[1] != m[2]:
            changed.append(m[0])
            if not check:
                m[0].write_text(m[2], encoding="utf-8")
        warn = []
        plain_t, plain_d = text(title), text(desc)
        if len(plain_t) > 65: warn.append("title > 65 chars (Google cuts it)")
        if not 70 <= len(plain_d) <= 160: warn.append("description should be 70-160 chars")
        if plain_t in titles: warn.append(f"same title as {titles[plain_t]}")
        titles[plain_t] = name
        if name == "index.html": warn.append(f"FAQ: {len(faq(new))} questions in JSON-LD") if faq(new) else warn.append("no FAQ found")
        report.append(f"  {name:18} title {len(plain_t):3}  desc {len(plain_d):3}  {'; '.join(warn)}")
    sm = SITE / "sitemap.xml"
    want = sitemap()
    if not sm.exists() or sm.read_text(encoding="utf-8") != want:
        changed.append(sm)
        if not check:
            sm.write_text(want, encoding="utf-8")
    print("\n".join(report))
    if changed:
        print(("out of date: " if check else "updated: ") + ", ".join(str(c.relative_to(ROOT.parent)) for c in changed))
    else:
        print("all up to date")
    return 1 if (check and changed) else 0


if __name__ == "__main__":
    sys.exit(main("--check" in sys.argv))
