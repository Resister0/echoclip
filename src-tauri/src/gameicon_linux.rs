//! Linux: a picture for each auto-created game folder (Windows takes it from the game's .exe in recorder.rs).
//! Sources, first hit wins:
//!   1. Steam's library icon (the game's current icon, if >= 32 px), else Steam's `steam_icon_<appid>.png`
//!   2. Lutris (`lutris_<slug>.png`) and Heroic (`heroic/icons/<app>.png`) icons
//!   3. the icon inside the game's .exe (Wine / Proton games, also non-Steam games added to Steam)
//!   4. a smaller Steam library icon / a non-Steam game's grid icon
//!   5. a .desktop file whose Name / StartupWMClass / Exec matches the game -> its Icon= (native games, Minecraft)
//! Written as a 64x64 PNG to `<app data>/icons/<safe name>.png` (the file the library and Settings > Games read).
//! Not in the Flatpak: the sandbox sees neither the game's process nor Steam's / Lutris' folders.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// "STAR WARS™ Battlefront II" -> "starwarsbattlefrontii" (for comparing names)
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// Lutris-style slug: "Counter-Strike 2" -> "counter-strike-2"
fn slug(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() {
            o.push(c);
        } else if !o.ends_with('-') {
            o.push('-');
        }
    }
    o.trim_matches('-').to_string()
}

/// Programs that run games but aren't the game (their name / .desktop file says nothing about it).
const GENERIC: &[&str] = &["java", "javaw", "wine", "wine64", "winepreloader", "wine64preloader", "wineserver", "python", "python3",
    "sh", "bash", "env", "flatpak", "steam", "mono", "dotnet", "electron", "node", "proton", "gamescope", "gamemoderun"];

/// What we know about the game.
#[derive(Default)]
struct Clues {
    /// Steam app ids (also the ids of non-Steam games added to Steam)
    appids: Vec<u64>,
    /// Steam folder names (steamapps/common/<dir>)
    steam_dirs: Vec<String>,
    /// the game's Windows .exe files on disk
    exes: Vec<PathBuf>,
    /// names to match (normalised): Echo's name for it, the exe name, Lutris' GAME_NAME
    names: Vec<String>,
    lutris: Vec<String>,
    heroic: Vec<String>,
}

impl Clues {
    fn id(&mut self, id: u64) {
        // a non-Steam game's 64-bit game id carries its app id in the top half
        let id = if id > u32::MAX as u64 { id >> 32 } else { id };
        if id != 0 && !self.appids.contains(&id) {
            self.appids.push(id);
        }
    }
    fn name(&mut self, n: &str) {
        let n = norm(n);
        if n.len() >= 2 && !GENERIC.contains(&n.as_str()) && !self.names.contains(&n) {
            self.names.push(n);
        }
    }
    fn exe(&mut self, p: PathBuf) {
        if p.is_file() && !self.exes.contains(&p) {
            self.exes.push(p);
        }
    }
    /// steamapps/common/<dir>/... in a path
    fn steam_path(&mut self, p: &str) {
        if let Some(r) = p.replace('\\', "/").split("/steamapps/common/").nth(1) {
            let d = r.split('/').next().unwrap_or("").to_string();
            let l = d.to_lowercase();
            if !d.is_empty() && !l.starts_with("proton") && !l.starts_with("steamlinuxruntime") && !l.starts_with("steamworks") && !self.steam_dirs.contains(&d) {
                self.steam_dirs.push(d);
            }
        }
    }
}

/// What the running game's process says: Steam ids from its environment, the .exe Wine has loaded, its folder.
fn from_process(pid: u32, c: &mut Clues) {
    let p = PathBuf::from(format!("/proc/{pid}"));
    if let Ok(env) = fs::read(p.join("environ")) {
        for kv in env.split(|b| *b == 0) {
            let kv = String::from_utf8_lossy(kv);
            let Some((k, v)) = kv.split_once('=') else { continue };
            match k {
                "SteamAppId" | "SteamGameId" | "STEAM_COMPAT_APP_ID" => {
                    if let Ok(id) = v.parse::<u64>() { c.id(id) }
                }
                // Proton: .../steamapps/compatdata/<appid>
                "STEAM_COMPAT_DATA_PATH" => {
                    if let Some(id) = Path::new(v).file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<u64>().ok()) { c.id(id) }
                }
                // Lutris runs games with GAME_NAME set
                "GAME_NAME" => {
                    c.lutris.push(slug(v));
                    c.name(v);
                }
                // ⚠ Heroic (not installed on the test PC)
                "HEROIC_APP_NAME" => c.heroic.push(v.to_string()),
                _ => {}
            }
        }
    }
    let a0 = fs::read(p.join("cmdline")).ok()
        .and_then(|b| b.split(|x| *x == 0).next().map(|a| String::from_utf8_lossy(a).replace('\\', "/")))
        .unwrap_or_default();
    let stem = Path::new(&a0).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    c.name(&stem);
    c.steam_path(&a0);
    if let Ok(x) = fs::read_link(p.join("exe")) {
        c.steam_path(&x.to_string_lossy());
    }
    // Wine maps the game's .exe into the process: its real path on disk (the one named like argv[0] first)
    if let Ok(maps) = fs::read_to_string(p.join("maps")) {
        let mut found: Vec<PathBuf> = Vec::new();
        for l in maps.lines() {
            let Some(i) = l.find('/') else { continue };
            let f = &l[i..];
            let fl = f.to_lowercase();
            if !fl.ends_with(".exe") || fl.contains("/windows/") || fl.contains("/lib/wine/") || fl.contains("/lib64/wine/") { continue; }
            let f = PathBuf::from(f);
            if !found.contains(&f) { found.push(f); }
        }
        found.sort_by_key(|f| f.file_stem().map_or(true, |s| s.to_string_lossy().to_lowercase() != stem));
        for f in found {
            c.steam_path(&f.to_string_lossy());
            c.exe(f);
        }
    }
}

/// Steam's folders (normal install, ~/.steam links, Flatpak Steam), each once.
fn steam_roots() -> Vec<PathBuf> {
    let h = home();
    let mut v: Vec<PathBuf> = Vec::new();
    for p in [h.join(".local/share/Steam"), h.join(".steam/steam"), h.join(".steam/root"),
        h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"), h.join(".var/app/com.valvesoftware.Steam/data/Steam")] {
        if let Ok(c) = p.canonicalize() {
            if !v.contains(&c) { v.push(c); }
        }
    }
    v
}

/// `"key"   "value"` lines of a Valve text file (.vdf / .acf)
fn vdf_values<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
    text.lines().filter_map(|l| {
        let p: Vec<&str> = l.trim().split('"').collect();
        (p.len() >= 4 && p[1].eq_ignore_ascii_case(key)).then(|| p[3])
    }).collect()
}

/// Every Steam library's steamapps folder.
fn steam_libraries(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if let Ok(c) = p.canonicalize() {
            if !v.contains(&c) { v.push(c); }
        }
    };
    for r in roots {
        add(r.join("steamapps"));
        if let Ok(t) = fs::read_to_string(r.join("steamapps/libraryfolders.vdf")) {
            for p in vdf_values(&t, "path") {
                add(Path::new(&p.replace("\\\\", "\\")).join("steamapps"));
            }
        }
    }
    v
}

/// Installed Steam games: (appid, name, installdir).
fn steam_manifests(libs: &[PathBuf]) -> Vec<(u64, String, String)> {
    let mut v = Vec::new();
    for l in libs {
        let Ok(rd) = fs::read_dir(l) else { continue };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if !n.starts_with("appmanifest_") || !n.ends_with(".acf") { continue; }
            let Ok(t) = fs::read_to_string(e.path()) else { continue };
            let get = |k: &str| vdf_values(&t, k).first().map(|s| s.to_string()).unwrap_or_default();
            if let Ok(id) = get("appid").parse::<u64>() {
                v.push((id, get("name"), get("installdir")));
            }
        }
    }
    v
}

/// "overwatch2" matches Steam's "Overwatch®" (a number after the name is fine), "counterstrike2" matches "Counter-Strike 2".
fn name_matches(game: &str, steam: &str) -> bool {
    if steam.len() < 3 { return false; }
    game == steam || (steam.len() >= 4 && game.starts_with(steam) && game.len() - steam.len() <= 2 && game[steam.len()..].chars().all(|c| c.is_ascii_digit()))
}

const SIZES: &[&str] = &["64x64", "96x96", "128x128", "256x256", "512x512", "48x48", "32x32", "24x24", "16x16"];

/// `<base>/<size>/apps/<file>` for the first size that has it (64 first, then bigger, then smaller).
fn sized(bases: &[PathBuf], file: &str) -> Option<PathBuf> {
    SIZES.iter().flat_map(|s| bases.iter().map(move |b| b.join(s).join("apps").join(file))).find(|p| p.is_file())
}

/// Steam's own icon: what its desktop shortcuts use (Steam writes it for every installed game).
fn steam_icon(id: u64) -> Option<PathBuf> {
    let h = home();
    sized(&[h.join(".local/share/icons/hicolor"), h.join(".var/app/com.valvesoftware.Steam/.local/share/icons/hicolor"),
        h.join(".var/app/com.valvesoftware.Steam/data/icons/hicolor")], &format!("steam_icon_{id}.png"))
}

/// Steam's library icon (the game's CURRENT icon, usually 32x32 - Steam's steam_icon_<appid>.png can be years old,
/// CS2 still had CS:GO's) and its size (smaller side; 0 = unknown).
fn steam_cache_icon(id: u64, roots: &[PathBuf]) -> Option<(PathBuf, u32)> {
    for r in roots {
        let lc = r.join("appcache/librarycache");
        let mut found = Some(lc.join(format!("{id}_icon.jpg"))).filter(|p| p.is_file());
        // newer Steam: librarycache/<appid>/<40-hex sha1>.jpg is the icon (pictures are in sub-folders)
        if found.is_none() {
            if let Ok(rd) = fs::read_dir(lc.join(id.to_string())) {
                found = rd.flatten().map(|e| e.path()).find(|p| {
                    let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    n.strip_suffix(".jpg").is_some_and(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())) && p.is_file()
                });
            }
        }
        if let Some(p) = found {
            let size = jpeg_size(&p).map_or(0, |(w, h)| w.min(h));
            return Some((p, size));
        }
    }
    None
}

/// Width and height of a JPEG (from its frame header, without decoding).
fn jpeg_size(p: &Path) -> Option<(u32, u32)> {
    let b = fs::read(p).ok()?;
    if !b.starts_with(&[0xff, 0xd8]) { return None; }
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xff { return None; }
        let m = b[i + 1];
        if m == 0xff { i += 1; continue; }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        // SOF0..SOF15 except DHT (c4), JPG (c8), DAC (cc)
        if (0xc0..=0xcf).contains(&m) && !matches!(m, 0xc4 | 0xc8 | 0xcc) {
            return Some((u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32, u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32));
        }
        i += 2 + len;
    }
    None
}

/// The grid icon picked in Steam for a (non-Steam) game.
fn steam_grid_icon(id: u64, roots: &[PathBuf]) -> Option<PathBuf> {
    for r in roots {
        if let Ok(rd) = fs::read_dir(r.join("userdata")) {
            for u in rd.flatten() {
                for ext in ["png", "jpg"] {
                    let g = u.path().join(format!("config/grid/{id}_icon.{ext}"));
                    if g.is_file() { return Some(g); }
                }
            }
        }
    }
    None
}

/// Lutris' icon for the game (its slug is the game's name, lowercase with dashes).
fn lutris_icon(c: &Clues, name: &str) -> Option<PathBuf> {
    let h = home();
    let mut slugs = c.lutris.clone();
    slugs.push(slug(name));
    for s in slugs.iter().filter(|s| !s.is_empty()) {
        let f = format!("lutris_{s}.png");
        if let Some(p) = sized(&[h.join(".local/share/icons/hicolor"), h.join(".var/app/net.lutris.Lutris/data/icons/hicolor")], &f) {
            return Some(p);
        }
        for d in [h.join(".local/share/lutris/icons"), h.join(".var/app/net.lutris.Lutris/data/lutris/icons")] {
            let p = d.join(format!("{s}.png"));
            if p.is_file() { return Some(p); }
        }
    }
    None
}

/// ⚠ Heroic (untested: not installed here): icons/<app name>.png; the app name from its store caches by title.
fn heroic_icon(c: &Clues) -> Option<PathBuf> {
    let h = home();
    for cfg in [h.join(".config/heroic"), h.join(".var/app/com.heroicgameslauncher.hgl/config/heroic")] {
        if !cfg.is_dir() { continue; }
        let mut apps = c.heroic.clone();
        fn walk(v: &serde_json::Value, names: &[String], out: &mut Vec<String>, depth: u32) {
            if depth > 6 { return; }
            match v {
                serde_json::Value::Object(m) => {
                    if let (Some(t), Some(a)) = (m.get("title").and_then(|x| x.as_str()), m.get("app_name").and_then(|x| x.as_str())) {
                        if names.contains(&norm(t)) { out.push(a.to_string()); }
                    }
                    for x in m.values() { walk(x, names, out, depth + 1); }
                }
                serde_json::Value::Array(a) => for x in a { walk(x, names, out, depth + 1); },
                _ => {}
            }
        }
        if let Ok(rd) = fs::read_dir(cfg.join("store_cache")) {
            for e in rd.flatten() {
                if e.path().extension().map_or(true, |x| x != "json") { continue; }
                if let Some(v) = fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) {
                    walk(&v, &c.names, &mut apps, 0);
                }
            }
        }
        for a in apps {
            for ext in ["png", "jpg", "jpeg", "webp"] {
                let p = cfg.join("icons").join(format!("{a}.{ext}"));
                if p.is_file() { return Some(p); }
            }
        }
    }
    None
}

/// An icon theme name (Icon=steam, Icon=com.mojang.Minecraft) -> its picture file.
fn theme_icon(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') {
        return Path::new(name).is_file().then(|| PathBuf::from(name));
    }
    let h = home();
    let bases: Vec<PathBuf> = [h.join(".local/share/icons"), h.join(".icons"), h.join(".local/share/flatpak/exports/share/icons"),
        PathBuf::from("/var/lib/flatpak/exports/share/icons"), PathBuf::from("/usr/local/share/icons"), PathBuf::from("/usr/share/icons")]
        .into_iter().map(|b| b.join("hicolor")).collect();
    if let Some(p) = sized(&bases, &format!("{name}.png")) { return Some(p); }
    for b in &bases {
        let p = b.join("scalable/apps").join(format!("{name}.svg"));
        if p.is_file() { return Some(p); }
    }
    ["png", "svg"].iter().map(|e| PathBuf::from(format!("/usr/share/pixmaps/{name}.{e}"))).find(|p| p.is_file())
}

/// A .desktop file (app menu entry) for the game: its Name, StartupWMClass or program matches.
fn desktop_icon(c: &Clues, exe_stems: &[String]) -> Option<PathBuf> {
    let h = home();
    let dirs = [h.join(".local/share/applications"), h.join(".local/share/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"), PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/usr/share/applications"), h.join("Desktop")];
    let mut files = Vec::new();
    fn collect(d: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 3 { collect(&p, depth + 1, out); }
            else if p.extension().map_or(false, |x| x == "desktop") { out.push(p); }
        }
    }
    for d in &dirs { collect(d, 0, &mut files); }
    // two passes: the name first (most certain), then the window class / program
    for pass in 0..2 {
        for f in &files {
            let Ok(t) = fs::read_to_string(f) else { continue };
            let mut in_entry = false;
            let (mut name, mut icon, mut exec, mut wm) = (String::new(), String::new(), String::new(), String::new());
            for l in t.lines() {
                let l = l.trim();
                if l.starts_with('[') { in_entry = l == "[Desktop Entry]"; continue; }
                if !in_entry { continue; }
                if let Some(v) = l.strip_prefix("Name=") { name = v.to_string(); }
                else if let Some(v) = l.strip_prefix("Icon=") { icon = v.to_string(); }
                else if let Some(v) = l.strip_prefix("Exec=") { exec = v.to_string(); }
                else if let Some(v) = l.strip_prefix("StartupWMClass=") { wm = v.to_string(); }
            }
            if icon.is_empty() { continue; }
            let hit = if pass == 0 {
                let n = norm(&name);
                !n.is_empty() && c.names.iter().any(|g| *g == n || format!("{g}launcher") == n)
            } else {
                let w = norm(&wm);
                // program of the Exec line (after env / VAR=x / options), and any .exe on it (Wine menu entries)
                let mut prog = String::new();
                let mut exes = Vec::new();
                let mut skip_next = false;
                for tok in exec.split_whitespace().map(|t| t.trim_matches('"').replace("\\\\", "/").replace('\\', "/")) {
                    if tok.to_lowercase().ends_with(".exe") {
                        exes.push(norm(Path::new(&tok).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default().as_str()));
                    }
                    if !prog.is_empty() { continue; }
                    if skip_next { skip_next = false; continue; }
                    if tok == "-u" { skip_next = true; continue; }
                    if tok == "env" || tok.starts_with('-') || tok.contains('=') { continue; }
                    prog = norm(Path::new(&tok).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default().as_str());
                }
                let any = |x: &str| !x.is_empty() && !GENERIC.contains(&x) && exe_stems.iter().any(|s| s == x);
                (!w.is_empty() && c.names.contains(&w)) || any(&prog) || exes.iter().any(|x| any(x))
            };
            if hit {
                if let Some(p) = theme_icon(&icon) { return Some(p); }
            }
        }
    }
    None
}

/// Wine prefixes / game folders to look for "<name>.exe" in when the game isn't running (start-up backfill).
fn exe_search_roots(libs: &[PathBuf]) -> Vec<PathBuf> {
    let h = home();
    let mut v = Vec::new();
    // Proton prefixes first (non-Steam games added to Steam), the big Steam game folders last
    for l in libs {
        if let Ok(rd) = fs::read_dir(l.join("compatdata")) {
            for e in rd.flatten() {
                v.push(e.path().join("pfx/drive_c/Program Files"));
                v.push(e.path().join("pfx/drive_c/Program Files (x86)"));
            }
        }
    }
    v.push(h.join("Games"));
    v.push(h.join(".wine/drive_c/Program Files"));
    v.push(h.join(".wine/drive_c/Program Files (x86)"));
    if let Ok(rd) = fs::read_dir(h.join(".var/app/com.usebottles.bottles/data/bottles/bottles")) {
        for e in rd.flatten() {
            v.push(e.path().join("drive_c/Program Files"));
            v.push(e.path().join("drive_c/Program Files (x86)"));
        }
    }
    // the anime game launchers (Genshin etc.) keep the game next to their prefix
    if let Ok(rd) = fs::read_dir(h.join(".var/app")) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("moe.launcher.") { v.push(e.path().join("data")); }
        }
    }
    v.extend(libs.iter().map(|l| l.join("common")));
    v.into_iter().filter(|p| p.is_dir()).collect()
}

/// "<name>.exe" under the roots (a few folders deep, Windows' own folders skipped, stops after ~100000 entries).
fn find_exe(roots: &[PathBuf], names: &[String]) -> Option<PathBuf> {
    let mut budget = 100_000u32;
    fn walk(d: &Path, depth: u32, names: &[String], budget: &mut u32) -> Option<PathBuf> {
        let Ok(rd) = fs::read_dir(d) else { return None };
        let mut subs = Vec::new();
        for e in rd.flatten() {
            if *budget == 0 { return None; }
            *budget -= 1;
            let n = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let l = n.to_lowercase();
                if depth < 4 && !matches!(l.as_str(), "windows" | "users" | "programdata" | "dosdevices" | "shadercache" | "compatdata" | "__installer" | "_commonredist" | "redist") {
                    subs.push(e.path());
                }
            } else if let Some(s) = n.strip_suffix(".exe").or_else(|| n.strip_suffix(".EXE")) {
                if names.contains(&norm(s)) { return Some(e.path()); }
            }
        }
        subs.into_iter().find_map(|s| walk(&s, depth + 1, names, budget))
    }
    roots.iter().find_map(|r| walk(r, 0, names, &mut budget))
}

/// A picture found inside an .exe.
enum Img {
    Png(Vec<u8>),
    Bgra(usize, usize, Vec<u8>),
}

/// The .exe's main icon (its first icon group, biggest picture) from the PE resources. Reads only the bits it needs.
fn pe_icon(path: &Path) -> Option<Img> {
    let f = fs::File::open(path).ok()?;
    let rd = |off: u64, n: usize| -> Option<Vec<u8>> {
        let mut b = vec![0u8; n];
        f.read_exact_at(&mut b, off).ok()?;
        Some(b)
    };
    let u16_ = |b: &[u8], o: usize| -> u16 { b.get(o..o + 2).map_or(0, |x| u16::from_le_bytes([x[0], x[1]])) };
    let u32_ = |b: &[u8], o: usize| -> u32 { b.get(o..o + 4).map_or(0, |x| u32::from_le_bytes([x[0], x[1], x[2], x[3]])) };
    let dos = rd(0, 64)?;
    if &dos[..2] != b"MZ" { return None; }
    let pe = u32_(&dos, 0x3c) as u64;
    let hdr = rd(pe, 24)?;
    if &hdr[..4] != b"PE\0\0" { return None; }
    let (nsec, optsz) = (u16_(&hdr, 6) as usize, u16_(&hdr, 20) as usize);
    let opt = rd(pe + 24, optsz)?;
    let dd = match u16_(&opt, 0) { 0x10b => 96, 0x20b => 112, _ => return None };
    let res_rva = u32_(&opt, dd + 16);
    if res_rva == 0 { return None; }
    let secs = rd(pe + 24 + optsz as u64, nsec * 40)?;
    let to_off = |rva: u32| -> Option<u64> {
        (0..nsec).find_map(|i| {
            let s = &secs[i * 40..];
            let (vs, va, rs, rp) = (u32_(s, 8) as u64, u32_(s, 12) as u64, u32_(s, 16) as u64, u32_(s, 20) as u64);
            let r = rva as u64;
            (r >= va && r < va + vs.max(rs)).then(|| r - va + rp)
        })
    };
    let root = to_off(res_rva)?;
    // a resource directory (offset from the root): (id, or None for a named entry; value)
    let entries = |off: u32| -> Vec<(Option<u32>, u32)> {
        let Some(d) = rd(root + off as u64, 16) else { return Vec::new() };
        let n = (u16_(&d, 12) as usize + u16_(&d, 14) as usize).min(4096);
        rd(root + off as u64 + 16, n * 8).map(|e| (0..n).map(|i| {
            let id = u32_(&e, i * 8);
            ((id & 0x8000_0000 == 0).then_some(id), u32_(&e, i * 8 + 4))
        }).collect()).unwrap_or_default()
    };
    // the data of an entry (its first language)
    let data = |v: u32| -> Option<Vec<u8>> {
        if v & 0x8000_0000 == 0 { return None; }
        let (_, leaf) = *entries(v & 0x7fff_ffff).first()?;
        if leaf & 0x8000_0000 != 0 { return None; }
        let de = rd(root + leaf as u64, 8)?;
        let size = u32_(&de, 4) as usize;
        if size == 0 || size > 4 << 20 { return None; }
        rd(to_off(u32_(&de, 0))?, size)
    };
    let types = entries(0);
    let ty = |id: u32| types.iter().find(|e| e.0 == Some(id)).map(|e| e.1).filter(|v| v & 0x8000_0000 != 0).map(|v| v & 0x7fff_ffff);
    let groups = entries(ty(14)?); // RT_GROUP_ICON
    let icons = entries(ty(3)?); // RT_ICON
    let grp = data(groups.first()?.1)?;
    let n = u16_(&grp, 4) as usize;
    // (width, bits per pixel, icon id): the biggest, most colours
    let best = (0..n).filter(|i| 6 + i * 14 + 14 <= grp.len()).map(|i| {
        let e = &grp[6 + i * 14..];
        (if e[0] == 0 { 256 } else { e[0] as u32 }, u16_(e, 6), u16_(e, 12) as u32)
    }).max_by_key(|&(w, bpp, _)| (w, bpp))?;
    let img = data(icons.iter().find(|e| e.0 == Some(best.2))?.1)?;
    decode_icon(&img)
}

/// One icon picture: PNG as it is, or a Windows bitmap (32 / 24 / 8 / 4 / 1 bit + see-through mask) -> BGRA pixels.
fn decode_icon(b: &[u8]) -> Option<Img> {
    if b.starts_with(b"\x89PNG") { return Some(Img::Png(b.to_vec())); }
    let rd32 = |o: usize| b.get(o..o + 4).map_or(0, |x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
    if b.len() < 40 { return None; }
    let (hs, w, h2) = (rd32(0) as usize, rd32(4) as i32, rd32(8) as i32);
    let bpp = u16::from_le_bytes([b[14], b[15]]) as usize;
    if rd32(16) != 0 || w <= 0 || w > 1024 || h2 <= 1 || h2 > 2048 || !matches!(bpp, 1 | 4 | 8 | 24 | 32) { return None; }
    let (w, h) = (w as usize, h2 as usize / 2); // the height counts the colour picture + the mask
    let ncol = if bpp <= 8 { match rd32(32) as usize { 0 => 1 << bpp, c => c.min(256) } } else { 0 };
    let px = hs + ncol * 4;
    let stride = (w * bpp + 31) / 32 * 4;
    let mask = px + stride * h;
    let mstride = (w + 31) / 32 * 4;
    if b.len() < mask { return None; }
    let mut out = vec![0u8; w * h * 4];
    let mut alpha = false;
    for y in 0..h {
        let row = &b[px + (h - 1 - y) * stride..px + (h - y) * stride];
        for x in 0..w {
            let c = match bpp {
                32 => [row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]],
                24 => [row[x * 3], row[x * 3 + 1], row[x * 3 + 2], 255],
                _ => {
                    let i = match bpp { 8 => row[x] as usize, 4 => ((row[x / 2] >> (if x % 2 == 0 { 4 } else { 0 })) & 15) as usize, _ => ((row[x / 8] >> (7 - x % 8)) & 1) as usize };
                    let p = hs + i * 4;
                    [*b.get(p)?, *b.get(p + 1)?, *b.get(p + 2)?, 255]
                }
            };
            if bpp == 32 && c[3] != 0 { alpha = true; }
            out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&c);
        }
    }
    // no alpha channel (or an empty one): the 1-bit mask says which pixels are see-through
    if !alpha {
        for y in 0..h {
            for x in 0..w {
                let clear = b.get(mask + (h - 1 - y) * mstride + x / 8).map_or(false, |m| (m >> (7 - x % 8)) & 1 == 1);
                out[(y * w + x) * 4 + 3] = if clear { 0 } else { 255 };
            }
        }
    }
    Some(Img::Bgra(w, h, out))
}

static TMP_N: AtomicU32 = AtomicU32::new(0);

/// Picture (file / PNG bytes / BGRA pixels) -> 64x64 PNG at `out` (kept in proportion, see-through padding).
fn write_png(src: Result<&Path, &Img>, out: &Path, ffmpeg: fn() -> Command) -> bool {
    let tmp = std::env::temp_dir().join(format!("echo_icon_{}_{}", std::process::id(), TMP_N.fetch_add(1, Ordering::Relaxed)));
    let mut cmd = ffmpeg();
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    let mut tmp_in: Option<PathBuf> = None;
    match src {
        Ok(p) => { cmd.arg("-i").arg(p); }
        Err(Img::Png(b)) => {
            let p = tmp.with_extension("png");
            if fs::write(&p, b).is_err() { return false; }
            cmd.arg("-i").arg(&p);
            tmp_in = Some(p);
        }
        Err(Img::Bgra(w, h, px)) => {
            let p = tmp.with_extension("raw");
            if fs::write(&p, px).is_err() { return false; }
            cmd.args(["-f", "rawvideo", "-pix_fmt", "bgra", "-s", &format!("{w}x{h}"), "-i"]).arg(&p);
            tmp_in = Some(p);
        }
    }
    // written next to the final file, then renamed: the library never loads half a picture
    let part = out.with_extension("part.png");
    let ok = cmd.args(["-frames:v", "1", "-vf",
        "scale=64:64:force_original_aspect_ratio=decrease:flags=lanczos,format=rgba,pad=64:64:(ow-iw)/2:(oh-ih)/2:color=0x00000000"])
        .arg(&part).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success())
        && fs::metadata(&part).map_or(false, |m| m.len() > 0)
        && fs::rename(&part, out).is_ok();
    if !ok { let _ = fs::remove_file(&part); }
    if let Some(p) = tmp_in { let _ = fs::remove_file(p); }
    ok
}

/// Find the game's icon and write it to `out` as a 64x64 PNG. `pid` = the running game (0 = not running:
/// found by its name / stored path only). Returns where the picture came from (for the log), None = nothing found.
pub fn make(name: &str, exe: &str, pid: u32, out: &Path, ffmpeg: fn() -> Command) -> Option<String> {
    if crate::linuxutil::in_flatpak() { return None; }
    let mut c = Clues::default();
    if pid > 0 { from_process(pid, &mut c); }
    c.name(name);
    c.steam_path(exe);
    if exe.to_lowercase().ends_with(".exe") { c.exe(PathBuf::from(exe)); }
    let exe_stem = Path::new(exe).file_stem().map(|s| norm(&s.to_string_lossy())).unwrap_or_default();
    let mut stems: Vec<String> = c.names.clone();
    if !exe_stem.is_empty() && !GENERIC.contains(&exe_stem.as_str()) && !exe_stem.contains("preloader") { stems.push(exe_stem); }

    let roots = steam_roots();
    let libs = steam_libraries(&roots);
    let mans = steam_manifests(&libs);
    // Steam folder name / game name -> app id
    for (id, n, dir) in &mans {
        let (nn, nd) = (norm(n), norm(dir));
        if nn.starts_with("proton") || nn.starts_with("steamlinuxruntime") || nn.starts_with("steamworks") { continue; }
        if c.steam_dirs.iter().any(|d| d.eq_ignore_ascii_case(dir)) || c.names.iter().any(|g| name_matches(g, &nn) || name_matches(g, &nd)) {
            c.id(*id);
        }
    }
    let to = |src: &Path| write_png(Ok(src), out, ffmpeg);
    // Steam: the library icon (current game art; shown at ~24 px, so 32x32 is sharp enough), else steam_icon_<appid>
    for id in c.appids.clone() {
        if let Some((p, size)) = steam_cache_icon(id, &roots).filter(|x| x.1 >= 32) {
            if to(&p) { return Some(format!("Steam library icon, {size} px ({})", p.display())); }
        }
        if let Some(p) = steam_icon(id) {
            if to(&p) { return Some(format!("Steam icon (app {id})")); }
        }
    }
    if let Some(p) = lutris_icon(&c, name) {
        if to(&p) { return Some(format!("Lutris ({})", p.display())); }
    }
    if let Some(p) = heroic_icon(&c) {
        if to(&p) { return Some(format!("Heroic ({})", p.display())); }
    }
    // the game's own .exe (not running: look for "<name>.exe" in Wine prefixes / Steam folders)
    if c.exes.is_empty() && (exe.contains("wine") || exe.contains("proton") || exe.to_lowercase().ends_with(".exe") || !c.appids.is_empty()) {
        let mut roots = Vec::new();
        for id in &c.appids {
            for (i, _, dir) in &mans {
                if i == id { roots.extend(libs.iter().map(|l| l.join("common").join(dir)).filter(|p| p.is_dir())); }
            }
        }
        if roots.is_empty() { roots = exe_search_roots(&libs); }
        if let Some(p) = find_exe(&roots, &c.names) { c.exe(p); }
    }
    for x in c.exes.clone() {
        if let Some(img) = pe_icon(&x) {
            if write_png(Err(&img), out, ffmpeg) { return Some(format!("icon in {}", x.display())); }
        }
    }
    for id in c.appids.clone() {
        if let Some(p) = steam_cache_icon(id, &roots).map(|x| x.0).or_else(|| steam_grid_icon(id, &roots)) {
            if to(&p) { return Some(format!("Steam library / grid icon ({})", p.display())); }
        }
    }
    if let Some(p) = desktop_icon(&c, &stems) {
        if to(&p) { return Some(format!("app menu entry icon ({})", p.display())); }
    }
    None
}
