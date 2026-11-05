//! Linux versions of the small Windows helpers in winutil.rs.

use std::io::{Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::sync::OnceLock;

static LISTENER: OnceLock<UnixListener> = OnceLock::new();

fn addr() -> Option<SocketAddr> {
    // abstract socket (no file): per user, gone when Echo exits
    let user = std::env::var("USER").unwrap_or_default();
    SocketAddr::from_abstract_name(format!("echo-clips-{user}").as_bytes()).ok()
}

/// Single instance: true if Echo already runs (it was asked to show its window, so just exit).
/// Otherwise this process now owns the socket.
pub fn already_running() -> bool {
    let Some(a) = addr() else { return false };
    if let Ok(mut s) = UnixStream::connect_addr(&a) {
        let _ = s.write_all(b"show");
        return true;
    }
    if let Ok(l) = UnixListener::bind_addr(&a) {
        let _ = LISTENER.set(l);
    }
    false
}

/// Calls `cb` whenever a second launch asks the running Echo to show itself.
pub fn listen_show(cb: impl Fn() + Send + 'static) {
    let Some(l) = LISTENER.get() else { return };
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut b = [0u8; 8];
            let mut s = s;
            let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(1)));
            if s.read(&mut b).map_or(false, |n| &b[..n] == b"show") {
                cb();
            }
        }
    });
}

/// Seconds since the last keyboard / mouse input (KDE / freedesktop screensaver D-Bus; 0 if unknown).
pub fn idle_seconds() -> u64 {
    use dbus::blocking::Connection;
    let Ok(c) = Connection::new_session() else { return 0 };
    let p = c.with_proxy("org.freedesktop.ScreenSaver", "/org/freedesktop/ScreenSaver", std::time::Duration::from_secs(2));
    let r: Result<(u32,), _> = p.method_call("org.freedesktop.ScreenSaver", "GetSessionIdleTime", ());
    r.map(|(ms,)| ms as u64 / 1000).unwrap_or(0)
}

/// Free bytes on the drive that holds `dir`.
pub fn disk_free(dir: &str) -> u64 {
    let o = crate::recorder::sys_cmd("df").args(["--output=avail", "-B1", dir]).output();
    o.ok().and_then(|o| String::from_utf8_lossy(&o.stdout).lines().nth(1).and_then(|l| l.trim().parse().ok())).unwrap_or(0)
}

static DND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);   // 0 unknown, 1 off, 2 on

/// Was the desktop's Do Not Disturb on when Echo last sent a notification? (the window's one-time tip)
pub fn dnd_on() -> bool {
    DND.load(std::sync::atomic::Ordering::Relaxed) == 2
}

/// Desktop notification (shows over games in KDE's corner; the Windows popup window can't on Wayland).
/// Straight over D-Bus (no notify-send needed - it isn't in the Flatpak); each new one replaces the last.
pub fn notify(title: &str, sub: &str, ok: bool) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static LAST: AtomicU32 = AtomicU32::new(0);
    let (title, sub) = (title.to_string(), sub.to_string());
    std::thread::spawn(move || {
        use dbus::arg::{PropMap, RefArg, Variant};
        let sent = (|| -> Option<u32> {
            let c = dbus::blocking::Connection::new_session().ok()?;
            let p = c.with_proxy("org.freedesktop.Notifications", "/org/freedesktop/Notifications", std::time::Duration::from_secs(3));
            // KDE turns on Do Not Disturb by itself while the screen is shared - and the Flatpak records THROUGH screen
            // sharing, so nothing pops up (chat 26). Remember it: the window shows a one-time tip (desktop_dnd).
            // ("critical" urgency gets past DND, but KDE keeps those up until clicked away - William said no.)
            use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
            if let Ok(on) = p.get::<bool>("org.freedesktop.Notifications", "Inhibited") { DND.store(if on { 2 } else { 1 }, Ordering::Relaxed); }
            let mut hints = PropMap::new();
            hints.insert("urgency".into(), Variant(Box::new(if ok { 0u8 } else { 1u8 }) as Box<dyn RefArg>));
            hints.insert("x-canonical-private-synchronous".into(), Variant(Box::new("echo".to_string()) as Box<dyn RefArg>));
            // which app sent it (KDE / GNOME group and allow notifications per app by this)
            hints.insert("desktop-entry".into(), Variant(Box::new(if in_flatpak() { "link.echoclip.Echo" } else { "echo" }.to_string()) as Box<dyn RefArg>));
            let icon = if in_flatpak() { "link.echoclip.Echo" } else { "echo-clips" };
            let (id,): (u32,) = p.method_call("org.freedesktop.Notifications", "Notify",
                ("Echo", LAST.load(Ordering::Relaxed), icon, title.as_str(), sub.as_str(), Vec::<String>::new(), hints, 2500i32))
                .map_err(|e| { static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if !SAID.swap(true, Ordering::Relaxed) { crate::recorder::log_line(&format!("notify: the desktop refused the notification: {e}")); } }).ok()?;
            Some(id)
        })();
        // every notification in the log (chat 25: none showed in the Flatpak, no error either)
        crate::recorder::log_line(&format!("notify: \"{title}\" -> {}", sent.map_or("not sent (no session bus?) - trying notify-send".to_string(), |id| format!("shown as #{id}{}",
            match DND.load(Ordering::Relaxed) { 2 => " - but the desktop's Do Not Disturb is ON (popups only in the history)", 1 => " (Do Not Disturb off)", _ => "" }))));
        match sent {
            Some(id) => LAST.store(id, Ordering::Relaxed),
            None => {   // no notification service on the bus: the old way
                let _ = crate::recorder::sys_cmd("notify-send")
                    .args(["-a", "Echo", "-i", "echo-clips", "-t", "2500", "-u", if ok { "low" } else { "normal" }, "-h", "string:x-canonical-private-synchronous:echo", &title, &sub])
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
            }
        }
    });
}

pub fn pid_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

// ---------------- Flatpak (Flathub / Bazaar build) ----------------

/// Running inside a Flatpak sandbox: no self-updates (the store updates Echo), system programs come from the
/// Flatpak itself, screen capture goes through the desktop's portal, pickers and autostart too.
pub fn in_flatpak() -> bool {
    static F: OnceLock<bool> = OnceLock::new();
    *F.get_or_init(|| std::env::var_os("FLATPAK_ID").is_some() || std::path::Path::new("/.flatpak-info").exists())
}

/// Waits for an xdg-desktop-portal Request's "Response" signal: (0 = ok, results). `call` starts the request
/// with the given handle_token and returns the request path the portal answered with.
fn portal_request<F>(handle_token: &str, timeout: std::time::Duration, call: F) -> Option<(u32, dbus::arg::PropMap)>
where F: FnOnce(&dbus::blocking::Connection) -> Result<dbus::Path<'static>, String> {
    use dbus::blocking::Connection;
    use dbus::message::MatchRule;
    use std::sync::{Arc, Mutex};
    let conn = Connection::new_session().ok()?;
    let sender = conn.unique_name().trim_start_matches(':').replace('.', "_");
    // listen BEFORE calling (the portal may answer at once); the path is predictable from the token
    let expect = format!("/org/freedesktop/portal/desktop/request/{sender}/{handle_token}");
    let got: Arc<Mutex<Option<(u32, dbus::arg::PropMap)>>> = Default::default();
    let g2 = got.clone();
    let rule = MatchRule::new_signal("org.freedesktop.portal.Request", "Response");
    conn.add_match(rule, move |_: (), _, msg| {
        if msg.path().map_or(false, |p| p.to_string() == expect) {
            if let (Some(code), Some(res)) = msg.get2::<u32, dbus::arg::PropMap>() { *g2.lock().unwrap() = Some((code, res)); }
        }
        true
    }).ok()?;
    if let Err(e) = call(&conn) {
        crate::recorder::log_line(&format!("portal: {e}"));
        return None;
    }
    let end = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < end {
        let _ = conn.process(std::time::Duration::from_millis(250));
        if let Some(r) = got.lock().unwrap().take() { return Some(r); }
    }
    None
}

fn token(what: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!("echo_{what}_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// File / folder picker through the desktop's FileChooser portal (works in the Flatpak, no kdialog/zenity needed).
/// `filter` = ("Videos", "*.mp4;*.mkv").
pub fn portal_pick(title: &str, filter: Option<(&str, &str)>, folder: bool) -> Option<String> {
    use dbus::arg::{PropMap, RefArg, Variant};
    let tok = token("pick");
    let (code, res) = portal_request(&tok, std::time::Duration::from_secs(3600), |conn| {
        let p = conn.with_proxy("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", std::time::Duration::from_secs(10));
        let mut opt = PropMap::new();
        opt.insert("handle_token".into(), Variant(Box::new(tok.clone()) as Box<dyn RefArg>));
        opt.insert("modal".into(), Variant(Box::new(true) as Box<dyn RefArg>));
        if folder { opt.insert("directory".into(), Variant(Box::new(true) as Box<dyn RefArg>)); }
        if let Some((name, pats)) = filter {
            let list: Vec<(u32, String)> = pats.split(';').map(|p| p.trim()).filter(|p| !p.is_empty()).map(|p| (0u32, p.to_string())).collect();
            let filters: Vec<(String, Vec<(u32, String)>)> = vec![(name.to_string(), list)];
            opt.insert("filters".into(), Variant(Box::new(filters) as Box<dyn RefArg>));
        }
        let (path,): (dbus::Path<'static>,) = p.method_call("org.freedesktop.portal.FileChooser", "OpenFile", ("", title, opt))
            .map_err(|e| format!("file chooser: {e}"))?;
        Ok(path)
    })?;
    if code != 0 { return None; }
    let uris = res.get("uris")?;
    let first = uris.0.as_iter()?.next()?.as_str()?.to_string();
    let path = first.strip_prefix("file://").map(url_decode)?;
    (!path.is_empty()).then_some(path)
}

/// Start with the computer, Flatpak style: the Background portal writes the autostart entry for us
/// (~/.config/autostart is outside the sandbox). Fire and forget - the desktop may ask the user once.
pub fn portal_autostart(on: bool) {
    std::thread::spawn(move || {
        use dbus::arg::{PropMap, RefArg, Variant};
        let tok = token("bg");
        let r = portal_request(&tok, std::time::Duration::from_secs(120), |conn| {
            let p = conn.with_proxy("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", std::time::Duration::from_secs(10));
            let mut opt = PropMap::new();
            opt.insert("handle_token".into(), Variant(Box::new(tok.clone()) as Box<dyn RefArg>));
            opt.insert("reason".into(), Variant(Box::new("Echo keeps recording in the background so you can save clips".to_string()) as Box<dyn RefArg>));
            opt.insert("autostart".into(), Variant(Box::new(on) as Box<dyn RefArg>));
            opt.insert("commandline".into(), Variant(Box::new(vec!["echo-clips".to_string(), "--autostart".to_string()]) as Box<dyn RefArg>));
            opt.insert("dbus-activatable".into(), Variant(Box::new(false) as Box<dyn RefArg>));
            let (path,): (dbus::Path<'static>,) = p.method_call("org.freedesktop.portal.Background", "RequestBackground", ("", opt))
                .map_err(|e| format!("background: {e}"))?;
            Ok(path)
        });
        let msg = match r {
            Some((0, res)) => format!("autostart {} (portal: background {}, autostart {})", if on { "on" } else { "off" },
                res.get("background").and_then(|v| v.0.as_u64()).map_or("?".into(), |b| (b != 0).to_string()),
                res.get("autostart").and_then(|v| v.0.as_u64()).map_or("?".into(), |b| (b != 0).to_string())),
            Some((c, _)) => format!("autostart: the desktop said no (portal answer {c})"),
            None => "autostart: no answer from the Background portal".into(),
        };
        crate::recorder::log_line(&format!("flatpak: {msg}"));
    });
}

// ---------------- Flatpak first start: one dialog at a time ----------------
// The sandbox asks the desktop for everything: shortcuts (KDE dialog) and screen recording (screen-share
// dialog). All of them at once on top of the Welcome screen was a mess (William, chat 25): the shortcut dialog
// waits for the Welcome screen, the screen-share dialog waits for the shortcut dialog.

static PORTALS_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SHORTCUTS_SETTLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// May the shortcut portal ask the user yet? (always outside the Flatpak)
pub fn portals_open() -> bool {
    !in_flatpak() || PORTALS_OPEN.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn open_portals() {
    PORTALS_OPEN.store(true, std::sync::atomic::Ordering::Relaxed);
}
/// The shortcut dialog was answered (OK or Cancel), or timed out.
pub fn set_shortcuts_settled() {
    if !SHORTCUTS_SETTLED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::recorder::log_line("flatpak: shortcut dialog answered - recording can ask for the screen now");
    }
}
/// Recording (= the screen-share dialog) may start: outside the Flatpak always.
pub fn flatpak_may_record() -> bool {
    !in_flatpak() || (portals_open() && SHORTCUTS_SETTLED.load(std::sync::atomic::Ordering::Relaxed))
}

/// Screenshot through the desktop's Screenshot portal (no dialog: interactive = false), saved as `out`.
/// KDE may ask once whether Echo may take screenshots. false = no answer / refused / failed.
pub fn portal_screenshot(out: &std::path::Path) -> bool {
    use dbus::arg::{PropMap, RefArg, Variant};
    let tok = token("shot");
    let Some((code, res)) = portal_request(&tok, std::time::Duration::from_secs(20), |conn| {
        let p = conn.with_proxy("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", std::time::Duration::from_secs(10));
        let mut opt = PropMap::new();
        opt.insert("handle_token".into(), Variant(Box::new(tok.clone()) as Box<dyn RefArg>));
        opt.insert("interactive".into(), Variant(Box::new(false) as Box<dyn RefArg>));
        let (path,): (dbus::Path<'static>,) = p.method_call("org.freedesktop.portal.Screenshot", "Screenshot", ("", opt))
            .map_err(|e| format!("screenshot: {e}"))?;
        Ok(path)
    }) else { return false };
    if code != 0 {
        crate::recorder::log_line(&format!("screenshot: the desktop said no (portal answer {code})"));
        return false;
    }
    let Some(uri) = res.get("uri").and_then(|v| v.0.as_str()).map(|s| s.to_string()) else { return false };
    let Some(src) = uri.strip_prefix("file://").map(url_decode) else { return false };
    // the portal saves it somewhere of its own (often ~/Pictures): move it into Echo's Screenshots folder
    let ok = std::fs::rename(&src, out).is_ok() || (std::fs::copy(&src, out).is_ok() && { let _ = std::fs::remove_file(&src); true });
    ok && std::fs::metadata(out).map_or(false, |m| m.len() > 0)
}

/// Flatpak: what the player's video engine has to work with, once at start (the player ran 16-19 fps at 1440p
/// in the sandbox, chat 25): GPU decode needs GStreamer's "va" plugin + a VA-API driver + /dev/dri.
pub fn flatpak_video_report() {
    std::thread::spawn(|| {
        let mut dirs: Vec<String> = std::env::var("GST_PLUGIN_SYSTEM_PATH").unwrap_or_default()
            .split(':').filter(|d| !d.is_empty()).map(String::from).collect();
        for d in ["/usr/lib/x86_64-linux-gnu/gstreamer-1.0", "/usr/lib/aarch64-linux-gnu/gstreamer-1.0", "/usr/lib/gstreamer-1.0",
                  "/app/lib/gstreamer-1.0", "/usr/lib/extensions/gstreamer-1.0"] {
            if !dirs.iter().any(|x| x == d) { dirs.push(d.into()); }
        }
        let has = |f: &str| dirs.iter().find(|d| std::path::Path::new(d).join(f).exists()).cloned();
        let plug: Vec<String> = [("va", "libgstva.so"), ("vaapi", "libgstvaapi.so"), ("libav", "libgstlibav.so"), ("openh264", "libgstopenh264.so")]
            .iter().map(|(n, f)| format!("{n} {}", has(f).map_or("no".to_string(), |d| format!("yes ({d})")))).collect();
        let mut drv: Vec<String> = Vec::new();
        for d in std::env::var("LIBVA_DRIVERS_PATH").unwrap_or_default().split(':').filter(|d| !d.is_empty()) {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().to_string();
                    if n.ends_with("_drv_video.so") { drv.push(format!("{n} in {d}")); }
                }
            }
        }
        let render = std::fs::read_dir("/dev/dri").map(|rd| rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("renderD")).count()).unwrap_or(0);
        crate::recorder::log_line(&format!("flatpak: player video - GStreamer plugins: {}; VA-API drivers: {}; GPU render nodes: {render}; WEBKIT_DMABUF_RENDERER_FORCE_SHM={}",
            plug.join(", "), if drv.is_empty() { "none found".to_string() } else { drv.join(", ") },
            std::env::var("WEBKIT_DMABUF_RENDERER_FORCE_SHM").unwrap_or_else(|_| "(not set)".into())));
    });
}

/// "Show in folder": the file manager opens the clip's folder with the clip selected. Straight over D-Bus
/// (FileManager1 - Dolphin, Nautilus…; dbus-send isn't in the Flatpak = the folder button did nothing there),
/// then the Flatpak's OpenURI portal (OpenDirectory). false = the caller falls back to xdg-open.
pub fn show_in_folder(path: &std::path::Path) -> bool {
    use dbus::blocking::Connection;
    let Ok(c) = Connection::new_session() else { return false };
    let how = (|| {
        if !path.is_dir() {
            let uri = format!("file://{}", path.to_string_lossy().replace('%', "%25").replace(' ', "%20").replace('#', "%23"));
            let p = c.with_proxy("org.freedesktop.FileManager1", "/org/freedesktop/FileManager1", std::time::Duration::from_secs(5));
            let r: Result<(), _> = p.method_call("org.freedesktop.FileManager1", "ShowItems", (vec![uri], ""));
            match r {
                Ok(()) => return Some("file manager".to_string()),
                Err(e) => crate::recorder::log_line(&format!("folder: file manager didn't answer ({e})")),
            }
        }
        if in_flatpak() {
            use std::os::fd::IntoRawFd;
            let f = std::fs::File::open(path).ok()?;
            let fd = unsafe { dbus::arg::OwnedFd::new(f.into_raw_fd()) };
            let p = c.with_proxy("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", std::time::Duration::from_secs(10));
            let mut opt = dbus::arg::PropMap::new();
            opt.insert("handle_token".into(), dbus::arg::Variant(Box::new(token("dir")) as Box<dyn dbus::arg::RefArg>));
            let r: Result<(dbus::Path<'static>,), _> = p.method_call("org.freedesktop.portal.OpenURI", "OpenDirectory", ("", fd, opt));
            match r {
                Ok(_) => return Some("portal".to_string()),
                Err(e) => crate::recorder::log_line(&format!("folder: portal didn't open it ({e})")),
            }
        }
        None
    })();
    if let Some(h) = &how { crate::recorder::log_line(&format!("folder: shown ({h})")); }
    how.is_some()
}

/// Which WebKitGTK + Mesa (graphics driver) this Echo runs on, once ~8 s after start: the Flatpak brings its
/// own (GNOME runtime + Flathub's Mesa), the normal build uses the system's - to compare when only one freezes.
pub fn lib_report() {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(8));
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
        let mut libs: Vec<String> = Vec::new();
        for l in maps.lines() {
            let Some(path) = l.split_whitespace().nth(5) else { continue };
            let name = path.rsplit('/').next().unwrap_or("");
            if (name.starts_with("libwebkit2gtk") || name.starts_with("libgallium") || name.contains("radeonsi") || name.starts_with("libEGL_mesa")
                || name.starts_with("libgtk-3") || name.starts_with("libgstreamer-1.0")) && !libs.iter().any(|x| x == path) {
                libs.push(path.to_string());
            }
        }
        crate::recorder::log_line(&format!("linux: libraries ({}): {}", if in_flatpak() { "Flatpak" } else { "system" },
            if libs.is_empty() { "none found".into() } else { libs.join(", ") }));
    });
}
