#!/usr/bin/env python3
"""Draws the hub's icons + share picture into brand-site/public. Run again after a brand rename (it reads BRAND from seo.py).

    python3 brand-site/make_images.py

Writes: favicon.svg, favicon.ico (16-48), icon-192.png, icon-512.png, apple-touch-icon.png (180), og.png (1200x630),
site.webmanifest. Needs Pillow (python3-pillow) and the Inter font (rsms-inter-fonts on Fedora/Nobara); without Inter it
falls back to Pillow's default font, which looks worse.
The mark: an open ring with a small dot in its gap, white on Echo's purple gradient tile (a family look with Echo's ring logo).
"""
import json, math, pathlib, sys
from PIL import Image, ImageDraw, ImageFont

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from seo import BRAND, SITE  # noqa: E402

BG, MUTED, DIM, LINE, PANEL = (13, 13, 16), (160, 160, 171), (107, 107, 118), (38, 38, 44), (22, 22, 26)
ACCENT2 = (157, 133, 255)
STOPS = [(0.0, (139, 108, 255)), (0.55, (106, 76, 245)), (1.0, (79, 123, 255))]   # --grad in style.css
FONTS = pathlib.Path("/usr/share/fonts/rsms-inter-fonts")

SVG = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">'
       '<stop offset="0" stop-color="#8b6cff"/><stop offset=".55" stop-color="#6a4cf5"/><stop offset="1" stop-color="#4f7bff"/>'
       '</linearGradient></defs><rect width="256" height="256" rx="72" fill="url(#g)"/>'
       '<circle cx="128" cy="128" r="72" fill="none" stroke="#fff" stroke-width="22" stroke-linecap="round" '
       'stroke-dasharray="372 80" transform="rotate(-13 128 128)"/><circle cx="179" cy="77" r="17" fill="#fff"/></svg>\n')


def font(name, size):
    try:
        return ImageFont.truetype(str(FONTS / name), size)
    except OSError:
        return ImageFont.load_default(size)


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def grad_at(t):
    for (t0, c0), (t1, c1) in zip(STOPS, STOPS[1:]):
        if t <= t1:
            return lerp(c0, c1, (t - t0) / (t1 - t0))
    return STOPS[-1][1]


def tile(size, rounded=True):
    """the logo tile at `size` px, drawn 4x bigger and scaled down for smooth edges"""
    S = size * 4
    g = Image.new("RGB", (S, S))
    px = g.load()
    for y in range(S):
        for x in range(S):
            px[x, y] = grad_at((x + y) / (2 * S - 2))
    k = S / 256
    d = ImageDraw.Draw(g)
    cx = cy = 128 * k
    r, w = 72 * k, 22 * k
    a0, a1 = 315 + 31.8, 315 - 31.8 + 360      # gap of 80 units along the ring, centred on the dot (upper right), like the SVG dasharray
    a = a0
    while a <= a1:                              # the ring as a row of dots = exact geometry with round ends
        ex, ey = cx + r * math.cos(math.radians(a)), cy + r * math.sin(math.radians(a))
        d.ellipse([ex - w / 2, ey - w / 2, ex + w / 2, ey + w / 2], fill="white")
        a += 0.25
    dx, dy, dr = 179 * k, 77 * k, 17 * k
    d.ellipse([dx - dr, dy - dr, dx + dr, dy + dr], fill="white")
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, S - 1, S - 1], radius=round(72 * k) if rounded else 0, fill=255)
    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    out.paste(g, (0, 0), mask)
    return out.resize((size, size), Image.LANCZOS)


def og():
    W, H = 1200, 630
    img = Image.new("RGB", (W, H), BG)
    glow = Image.new("RGB", (W, H), BG)
    gp = glow.load()
    for y in range(0, H):
        for x in range(0, W):
            dd = math.hypot((x - 300) / 620, (y - 250) / 420)
            t = max(0.0, 1 - dd) ** 2 * 0.32
            gp[x, y] = lerp(BG, (124, 92, 255), t)
    img.paste(glow)
    d = ImageDraw.Draw(img)
    logo = tile(150)
    img.paste(logo, (90, 120), logo)
    d.text((270, 118), BRAND, font=font("InterDisplay-Bold.ttf", 104), fill=(245, 245, 247))
    d.text((274, 242), "Free, lightweight tools for PC gamers", font=font("Inter-Medium.ttf", 40), fill=MUTED)
    chips = [("Echo", "clip recorder · out now", True), ("Crosshair", "coming soon", False), ("Snip", "coming soon", False)]
    x, y = 90, 360
    for name, sub, live in chips:
        f1, f2 = font("Inter-Bold.ttf", 34), font("Inter-Medium.ttf", 24)
        w = max(d.textlength(name, font=f1), d.textlength(sub, font=f2)) + 56
        d.rounded_rectangle([x, y, x + w, y + 112], radius=20, fill=PANEL, outline=ACCENT2 if live else LINE, width=3 if live else 2)
        d.text((x + 28, y + 18), name, font=f1, fill=(245, 245, 247))
        d.text((x + 28, y + 66), sub, font=f2, fill=(52, 211, 153) if live else DIM)
        x += w + 20
    d.text((90, 530), "Free  ·  No ads  ·  Open source  ·  Windows & Linux", font=font("Inter-SemiBold.ttf", 30), fill=ACCENT2)
    return img


def main():
    (SITE / "favicon.svg").write_text(SVG, encoding="utf-8")
    tile(512).save(SITE / "icon-512.png", optimize=True)
    tile(192).save(SITE / "icon-192.png", optimize=True)
    tile(180, rounded=False).convert("RGB").save(SITE / "apple-touch-icon.png", optimize=True)   # iOS rounds it itself
    tile(256).save(SITE / "favicon.ico", sizes=[(16, 16), (32, 32), (48, 48)])
    og().save(SITE / "og.png", optimize=True)
    manifest = {"name": BRAND, "short_name": BRAND, "start_url": "/", "display": "browser",
                "background_color": "#0d0d10", "theme_color": "#7c5cff",
                "icons": [{"src": "/icon-192.png", "sizes": "192x192", "type": "image/png"},
                          {"src": "/icon-512.png", "sizes": "512x512", "type": "image/png"}]}
    (SITE / "site.webmanifest").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print("wrote favicon.svg, favicon.ico, icon-192/512.png, apple-touch-icon.png, og.png, site.webmanifest")


if __name__ == "__main__":
    main()
