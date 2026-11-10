#!/usr/bin/env python3
"""SEO upkeep for the Brandname hub site (brand-site/public). Same approach as cloud/site-src/seo.py for echoclip.link.

    python3 brand-site/seo.py          # update public/*.html heads + public/sitemap.xml, print a short report
    python3 brand-site/seo.py --check  # change nothing; exit 1 if something is out of date

It only touches the <head> (between <!-- seo --> and <!-- /seo -->) and sitemap.xml, never the visible page text:
- reads each page's <title> and <meta name="description"> (edit those by hand, they are the source of truth)
- writes canonical, Open Graph + Twitter tags, icons + manifest, and JSON-LD (home: WebSite + Organization + the apps as an
  ItemList; about: AboutPage). 404.html gets noindex instead.
- writes sitemap.xml (lastmod = the page's last git commit date)

Brand or domain changing? Edit BRAND / ORIGIN here, rename the brand in public/*.html, then run this and make_images.py.
"""
import datetime, html, json, pathlib, re, subprocess, sys

BRAND = "Brandname"
ORIGIN = "https://brandname.com"
SITE = pathlib.Path(__file__).resolve().parent / "public"
OG_IMAGE = f"{ORIGIN}/og.png?v=1"   # Discord caches images per URL: bump v when og.png changes
OG_ALT = f"{BRAND} - free, lightweight tools for PC gamers: Echo, Crosshair and Snip."
SAME_AS = ["https://github.com/Resister0/echoclip", "https://discord.gg/JPVewEfUQs"]
EMAIL = "support@echoclip.link"

# page file -> public path (Workers static assets serve about.html at /about). Order = sitemap order.
PAGES = {"index.html": "/", "about.html": "/about", "privacy.html": "/privacy"}
NOINDEX = ["404.html"]
PRIORITY = {"/": "1.0", "/about": "0.6", "/privacy": "0.3"}
# the apps, in the order of the homepage cards (JSON-LD ItemList). Planned apps get no URL until they have a page.
APPS = [
    {"name": "Echo", "url": "https://echoclip.link/", "about": "Game clip recorder for Windows and Linux"},
    {"name": f"{BRAND} Crosshair", "about": "Crosshair overlay (coming soon)"},
    {"name": f"{BRAND} Snip", "about": "Screenshot and snipping tool (coming soon)"},
]

BEGIN, END = "<!-- seo: managed by brand-site/seo.py, edit there -->", "<!-- /seo -->"


def text(s):
    return re.sub(r"\s+", " ", html.unescape(re.sub(r"<[^>]+>", "", s))).strip()


def head_info(page):
    t = re.search(r"<title>(.*?)</title>", page, re.S)
    d = re.search(r'<meta name="description" content="([^"]*)">', page)
    assert t and d, "page needs <title> and <meta name=\"description\" content=\"...\">"
    return t[1].strip(), d[1].strip()


def jsonld(name, desc):
    org = {"@type": "Organization", "@id": f"{ORIGIN}/#org", "name": BRAND, "url": f"{ORIGIN}/",
           "logo": f"{ORIGIN}/icon-512.png", "email": EMAIL, "sameAs": SAME_AS}
    if name == "index.html":
        items = []
        for i, a in enumerate(APPS, 1):
            sw = {"@type": "SoftwareApplication", "name": a["name"], "description": a["about"],
                  "operatingSystem": "Windows, Linux", "isAccessibleForFree": True,
                  "publisher": {"@id": f"{ORIGIN}/#org"}}
            if "url" in a:
                sw["url"] = a["url"]
            items.append({"@type": "ListItem", "position": i, "item": sw})
        graph = [{"@type": "WebSite", "@id": f"{ORIGIN}/#site", "name": BRAND, "url": f"{ORIGIN}/", "description": text(desc),
                  "publisher": {"@id": f"{ORIGIN}/#org"}}, org,
                 {"@type": "ItemList", "@id": f"{ORIGIN}/#apps", "name": f"Apps by {BRAND}", "itemListElement": items}]
    elif name == "about.html":
        graph = [org, {"@type": "AboutPage", "url": f"{ORIGIN}/about", "name": f"About {BRAND}", "about": {"@id": f"{ORIGIN}/#org"}}]
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
        f'<meta property="og:site_name" content="{BRAND}">',
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
    ld = "" if name in NOINDEX else jsonld(name, desc)
    return "\n".join(lines) + "\n" + ld + END + "\n"


def update(name, page):
    h_end = page.index("</head>")
    head, rest = page[:h_end], page[h_end:]
    head = re.sub(re.escape(BEGIN) + r".*?" + re.escape(END) + r"\n", "", head, flags=re.S)
    new = block(name, page)
    at = head.find('<link rel="stylesheet"')
    head = head[:at] + new + head[at:] if at >= 0 else head + new
    return head + rest


def lastmod(path):
    try:
        git = lambda *a: subprocess.run(["git", *a, "--", str(path)], cwd=SITE, capture_output=True, text=True).stdout.strip()
        d = "" if git("status", "--porcelain") else git("log", "-1", "--format=%cs")   # edited, not committed yet = today
    except OSError:
        d = ""
    return d or datetime.date.today().isoformat()


def sitemap():
    rows = [f"  <url><loc>{ORIGIN}{p}</loc><lastmod>{lastmod(SITE / f)}</lastmod><priority>{PRIORITY.get(p, '0.4')}</priority></url>"
            for f, p in PAGES.items()]
    return '<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n' + "\n".join(rows) + "\n</urlset>\n"


def robots():
    return (f"# {BRAND} - free, lightweight tools for PC gamers. Everything here may be indexed.\n"
            f"User-agent: *\nAllow: /\n\nSitemap: {ORIGIN}/sitemap.xml\n")


def main(check=False):
    changed, report, titles = [], [], {}
    for name in list(PAGES) + NOINDEX:
        f = SITE / name
        page = f.read_text(encoding="utf-8")
        new = update(name, page)
        if new != page:
            changed.append(f)
            if not check:
                f.write_text(new, encoding="utf-8")
        title, desc = head_info(new)
        warn = []
        plain_t, plain_d = text(title), text(desc)
        if len(plain_t) > 65: warn.append("title > 65 chars (Google cuts it)")
        if not 70 <= len(plain_d) <= 160: warn.append("description should be 70-160 chars")
        if plain_t in titles: warn.append(f"same title as {titles[plain_t]}")
        if BRAND not in plain_t: warn.append(f"title doesn't say {BRAND}")
        titles[plain_t] = name
        report.append(f"  {name:13} title {len(plain_t):3}  desc {len(plain_d):3}  {'; '.join(warn)}")
    for f, want in ((SITE / "sitemap.xml", sitemap()), (SITE / "robots.txt", robots())):
        if not f.exists() or f.read_text(encoding="utf-8") != want:
            changed.append(f)
            if not check:
                f.write_text(want, encoding="utf-8")
    print("\n".join(report))
    if changed:
        print(("out of date: " if check else "updated: ") + ", ".join(c.name for c in changed))
    else:
        print("all up to date")
    return 1 if (check and changed) else 0


if __name__ == "__main__":
    sys.exit(main("--check" in sys.argv))
