# brand-site: the Brandname hub

The small static site that lists Resister0's free gaming tools: Echo (echoclip.link, out now), Brandname Crosshair and Brandname Snip (coming soon).
Plain HTML + CSS in `public/`, no scripts, no cookies. Same look as echoclip.link.

- `public/` - the pages (index, about, privacy, 404), style.css, icons, og.png, robots.txt, sitemap.xml, _headers
- `seo.py` - after changing a page's `<title>` or description: `python3 brand-site/seo.py` (writes the SEO block in each head + sitemap.xml + robots.txt). `--check` only reports.
- `make_images.py` - redraws the icons, og.png and site.webmanifest (Pillow + Inter font)
- `wrangler.toml` - deploys `public/` as a static-assets-only Cloudflare Worker: `cd brand-site && npx wrangler deploy`
