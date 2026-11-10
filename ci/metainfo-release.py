#!/usr/bin/env python3
# Adds this publish as a <release> to flatpak/link.echoclip.Echo.metainfo.xml (Flathub / GNOME Software / Discover
# show it as "What's new"). Run by ci/build.yml in the PUBLIC copy of the code, so the file in the private repo stays
# short; the older entries come from the public repo's previous copy of the file.
# Usage: metainfo-release.py <metainfo.xml> <previous public metainfo.xml or ""> <version> <publish text>
import datetime, html, re, sys

path, old_path, version, text = sys.argv[1:5]
RE = re.compile(r"[ \t]*<release\b[^>]*\bversion=\"([^\"]+)\"[^>]*?(?:/>|>.*?</release>)[ \t]*\n?", re.S)

def entries(s):
    return {m.group(1): m.group(0).rstrip() for m in RE.finditer(s)}

def vkey(v):
    return tuple(int(x) if x.isdigit() else 0 for x in re.split(r"[.]", v))

s = open(path, encoding="utf-8").read()
rel = entries(s)
if old_path:
    try:
        for v, e in entries(open(old_path, encoding="utf-8").read()).items():
            rel.setdefault(v, e)
    except OSError:
        pass

# 9 Nov 2026: 2.0.72 was an internal test build before the real 2.0.0 (numbering restarted at 2.0.0) - sorted by
# version it would stay on top as the "newest" release
rel.pop("2.0.72", None)

text = " ".join(text.split())
# "#..." = internal build (hidden in Echo's What's new too)
if not text or text.startswith("#"):
    text = "Bug fixes and improvements"
if not text.endswith((".", "!", "?")):
    text += "."
today = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
rel[version] = (f'    <release version="{version}" date="{today}">\n'
                f"      <description>\n        <p>{html.escape(text, quote=False)}</p>\n      </description>\n"
                f"    </release>")

body = "\n".join(rel[v] for v in sorted(rel, key=vkey, reverse=True)[:30])
s2, n = re.subn(r"<releases>.*?</releases>", lambda _: "<releases>\n" + body + "\n  </releases>", s, count=1, flags=re.S)
if n != 1:
    sys.exit("no <releases> in " + path)
open(path, "w", encoding="utf-8").write(s2)
print(f"metainfo: release {version} added ({len(rel)} releases)")
