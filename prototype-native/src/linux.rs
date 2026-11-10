//! Linux-only pieces (gaps-linux, 10 Nov 2026; rows 38/39/52/53 of NATIVE_GAPS.md + the AppImage menu entry +
//! the Flatpak-safe file pickers). Windows never compiles this file.
//! ⚠ Not compiled for Linux on the PC it was written on - see Claude outputs/gaps_done_linux.md for the test list.
use crate::AppWindow;
use echo_core::linuxutil;
use echo_core::recorder::{log_line, sys_cmd};
use slint::ComponentHandle;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------- start

/// First thing in main() (rec_core::early_env), before any thread exists.
pub fn early() {
    // AppImage: programs we start (ffmpeg, kdialog, xdg-open, wl-copy, gio…) must not load the AppImage's own
    // libraries. Echo's recorder does this per command (sys_cmd); the native app's many Command::new calls didn't -
    // so clean this process' environment once. ld.so read LD_LIBRARY_PATH at start already, so our own libmpv
    // loading is unchanged.
    for (k, v) in echo_core::recorder::appimage_env_fixes() {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
    desktop_entry();
}

/// Port of src-tauri main.rs linux_desktop_entry(): the app menu entry + icon in the user's own folders,
/// rewritten only when they change. The old Tauri app wrote StartupWMClass=link.echoclip.Echo; the native window's
/// id is "echo" (popup.rs), so after the auto-update from Tauri KDE showed a generic taskbar icon. Same file name
/// as before = no second menu entry. Only the AppImage gets a visible entry (tar.gz / own build: hidden, it just
/// gives the window its icon). The Flatpak brings its own.
fn desktop_entry() {
    if linuxutil::in_flatpak() {
        return;
    }
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from) else { return };
    let data = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    let icon = data.join("icons/hicolor/256x256/apps/link.echoclip.Echo.png");
    let png: &[u8] = include_bytes!("../../src-tauri/icons/icon.png");
    if std::fs::read(&icon).map_or(true, |b| b != png) {
        if let Some(d) = icon.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&icon, png);
    }
    let appimage = std::env::var("APPIMAGE").ok().filter(|p| !p.is_empty());
    let exe = appimage.clone().unwrap_or_else(|| std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());
    if exe.is_empty() {
        return;
    }
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Echo\nComment=Game clips and instant replays\nExec=\"{exe}\"\nIcon=link.echoclip.Echo\nStartupWMClass=link.echoclip.Echo\nCategories=AudioVideo;Video;Game;\nTerminal=false\n{}",
        if appimage.is_some() { "" } else { "NoDisplay=true\n" }
    );
    let f = data.join("applications/link.echoclip.Echo.desktop");
    if std::fs::read_to_string(&f).map_or(true, |s| s != entry) {
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        match std::fs::write(&f, entry) {
            Ok(()) => log_line("linux: app menu entry written (StartupWMClass=link.echoclip.Echo)"),
            Err(e) => log_line(&format!("linux: couldn't write the app menu entry: {e}")),
        }
    }
}

/// Once the window exists (rec_core::wire): the "Echo needs ffmpeg" dialog when there is none.
pub fn wire(a: &AppWindow) {
    let w = a.as_weak();
    std::thread::spawn(move || {
        // after the Welcome screen, like every other first-run question
        while !setup_done() {
            std::thread::sleep(Duration::from_secs(2));
        }
        std::thread::sleep(Duration::from_secs(3));
        if crate::sheets::ffmpeg().is_some() {
            return;
        }
        log_line("ffmpeg: not found (ECHO_FFMPEG, next to Echo, PATH, /usr/bin, /usr/local/bin, ~/.local/bin) - telling the user");
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(a) = w.upgrade() {
                no_ffmpeg_dialog(&a);
            }
        });
    });
}

/// Echo's settings.json says the Welcome screen is done (read straight from the file: cheap, no keyring).
fn setup_done() -> bool {
    std::fs::read_to_string(crate::settings::path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("setup_done").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

// ---------------------------------------------------------------- row 38: ffmpeg

/// sheets::ffmpeg() on Linux after ECHO_FFMPEG. The old Linux Echo (AppImage / tar.gz) never downloaded ffmpeg -
/// it used the system's (the Flatpak has its own in /app/bin). Same here, but also: a copy next to Echo (a future
/// bundled one), and the usual folders a desktop/menu start may leave out of PATH. Every candidate is test-run
/// once (a found-but-broken ffmpeg is logged and skipped). Returns an absolute path.
pub fn find_ffmpeg() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(d.join("tools"));
        dirs.push(d);
    }
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    for d in ["/app/bin", "/usr/bin", "/usr/local/bin", "/snap/bin", "/home/linuxbrew/.linuxbrew/bin"] {
        dirs.push(d.into());
    }
    if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from) {
        dirs.push(h.join(".local/bin"));
        dirs.push(h.join("bin"));
    }
    let mut tried: Vec<PathBuf> = Vec::new();
    for d in dirs {
        let f = d.join("ffmpeg");
        if tried.contains(&f) || !is_exec(&f) {
            continue;
        }
        tried.push(f.clone());
        let ok = sys_cmd(&f.to_string_lossy()).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        if ok {
            return Some(f);
        }
        log_line(&format!("ffmpeg: {} is there but doesn't run - skipped", f.display()));
    }
    None
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The install command for this distro (from /etc/os-release), "" = unknown distro.
fn ffmpeg_install_cmd() -> &'static str {
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let mut ids = String::new();
    for l in os.lines() {
        if let Some(v) = l.strip_prefix("ID=").or_else(|| l.strip_prefix("ID_LIKE=")) {
            ids.push(' ');
            ids.push_str(&v.trim_matches('"').to_lowercase());
        }
    }
    let has = |w: &str| ids.split_whitespace().any(|x| x == w);
    if has("fedora") || has("nobara") || has("rhel") {
        "sudo dnf install ffmpeg-free"
    } else if has("debian") || has("ubuntu") {
        "sudo apt install ffmpeg"
    } else if has("arch") {
        "sudo pacman -S ffmpeg"
    } else if has("suse") || has("opensuse") {
        "sudo zypper install ffmpeg"
    } else {
        ""
    }
}

fn no_ffmpeg_dialog(a: &AppWindow) {
    let cmd = ffmpeg_install_cmd();
    let how = if cmd.is_empty() {
        "Install the ffmpeg package from your distro (Fedora / Nobara: ffmpeg-free, Ubuntu / Debian / Arch: ffmpeg), then restart Echo.".to_string()
    } else {
        format!("Install it, then restart Echo:\n{cmd}")
    };
    let msg = format!("Saving clips, thumbnails, the seek pictures, the editor and exports all use ffmpeg, and it isn't on this PC.\n\n{how}");
    let mut choices: Vec<(&str, &str, &str)> = Vec::new();
    if !cmd.is_empty() {
        choices.push(("Copy command", "copy", ""));
    }
    choices.push(("Got it", "ok", "primary"));
    crate::ask(a, "Echo needs ffmpeg", &msg, &choices, None, "ok", move |a, v, _| {
        if v == "copy" {
            match crate::lib_net::copy_text(cmd) {
                Ok(()) => crate::toast(a, "Command copied - paste it in a terminal"),
                Err(e) => crate::toast(a, e),
            }
        }
    });
}

// ---------------------------------------------------------------- row 39: Trash without gio

/// Delete = Trash, never a permanent delete. gio first (GLib: the home Trash, other drives, and the Trash portal
/// inside the Flatpak); without gio the desktop's Trash portal (Flatpak) or the freedesktop Trash spec by hand
/// (~/.local/share/Trash, or $topdir/.Trash/$uid / $topdir/.Trash-$uid on other drives). If none works the file
/// stays and the caller shows the error - the old code deleted it for good while the toast said "Trash".
pub fn trash(p: &Path) -> Result<(), String> {
    let gone = |p: &Path| std::fs::symlink_metadata(p).is_err();
    let gio = sys_cmd("gio").arg("trash").arg(p).stdout(Stdio::null()).stderr(Stdio::null()).status();
    if gio.as_ref().is_ok_and(|s| s.success()) && gone(p) {
        return Ok(());
    }
    let r = if linuxutil::in_flatpak() { portal_trash(p) } else { spec_trash(p) };
    match r {
        Ok(()) if gone(p) => Ok(()),
        Ok(()) => Err("Couldn't move it to the Trash - nothing was deleted".into()),
        Err(e) => {
            log_line(&format!("trash: {} -> {e} (gio: {})", p.display(), match &gio { Ok(s) => s.to_string(), Err(e) => format!("not found ({e})") }));
            Err(format!("Couldn't move it to the Trash, so nothing was deleted ({e})"))
        }
    }
}

/// The Flatpak's Trash portal (org.freedesktop.portal.Trash.TrashFile: 1 = done).
fn portal_trash(p: &Path) -> Result<(), String> {
    use std::os::fd::IntoRawFd;
    let f = std::fs::File::open(p).map_err(|e| e.to_string())?;
    let fd = unsafe { dbus::arg::OwnedFd::new(f.into_raw_fd()) };
    let c = dbus::blocking::Connection::new_session().map_err(|e| e.to_string())?;
    let px = c.with_proxy("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", Duration::from_secs(20));
    let (r,): (u32,) = px.method_call("org.freedesktop.portal.Trash", "TrashFile", (fd,)).map_err(|e| format!("the desktop's Trash didn't answer: {e}"))?;
    if r == 1 {
        Ok(())
    } else {
        Err("the desktop's Trash said no".into())
    }
}

/// The freedesktop.org Trash spec 1.0, files only (Echo deletes clips and screenshots, never folders).
fn spec_trash(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let meta = std::fs::symlink_metadata(p).map_err(|e| e.to_string())?;
    let name = p.file_name().ok_or("no file name")?.to_owned();
    let parent = std::fs::canonicalize(p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    let abs = parent.join(&name);
    let dev = meta.dev();
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).map_err(|e| e.to_string())?;
    let mkdir = |d: &Path| std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d);

    // 1. the home Trash, when the file is on the same drive (a rename, never a copy of a big clip)
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let data = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()).map(PathBuf::from).or_else(|| home.map(|h| h.join(".local/share")));
    let mut base: Option<(PathBuf, String)> = None; // (trash dir, Path= value)
    if let Some(data) = data {
        let mut probe = data.clone();
        while std::fs::metadata(&probe).is_err() {
            if !probe.pop() {
                break;
            }
        }
        if std::fs::metadata(&probe).is_ok_and(|m| m.dev() == dev) {
            base = Some((data.join("Trash"), abs.to_string_lossy().into_owned()));
        }
    }
    // 2. another drive: its top folder's .Trash/$uid (admin-made, sticky, not a link), else .Trash-$uid
    if base.is_none() {
        let mut top = parent.clone();
        while let Some(up) = top.parent() {
            if std::fs::metadata(up).map_or(true, |m| m.dev() != dev) {
                break;
            }
            top = up.to_path_buf();
        }
        let rel = abs.strip_prefix(&top).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| abs.to_string_lossy().into_owned());
        let shared = top.join(".Trash");
        let shared_ok = std::fs::symlink_metadata(&shared).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink() && m.mode() & 0o1000 != 0);
        let mine = shared.join(uid.to_string());
        if shared_ok && mkdir(&mine).is_ok() {
            base = Some((mine, rel));
        } else {
            let own = top.join(format!(".Trash-{uid}"));
            match std::fs::symlink_metadata(&own) {
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() && m.uid() == uid => base = Some((own, rel)),
                Ok(_) => return Err(format!("{} isn't a usable Trash folder", own.display())),
                Err(_) => {
                    mkdir(&own).map_err(|e| format!("no Trash on this drive ({e})"))?;
                    base = Some((own, rel));
                }
            }
        }
    }
    let (trash, orig) = base.ok_or("no Trash folder")?;
    let (files, info) = (trash.join("files"), trash.join("info"));
    mkdir(&files).map_err(|e| e.to_string())?;
    mkdir(&info).map_err(|e| e.to_string())?;

    // a free name: "clip.mp4", then "clip.2.mp4", "clip.3.mp4"… - the .trashinfo is made first with create_new,
    // which reserves the name (spec)
    let n = name.to_string_lossy().into_owned();
    let (stem, ext) = match n.rfind('.') {
        Some(i) if i > 0 => (n[..i].to_string(), n[i..].to_string()),
        _ => (n.clone(), String::new()),
    };
    let body = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", url_escape(&orig), chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"));
    for k in 1..1000u32 {
        let cand = if k == 1 { n.clone() } else { format!("{stem}.{k}{ext}") };
        let inf = info.join(format!("{cand}.trashinfo"));
        let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(&inf) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        };
        let dest = files.join(&cand);
        if std::fs::symlink_metadata(&dest).is_ok() {
            drop(f);
            let _ = std::fs::remove_file(&inf);
            continue;
        }
        use std::io::Write;
        if let Err(e) = f.write_all(body.as_bytes()).and_then(|_| f.sync_all()) {
            drop(f);
            let _ = std::fs::remove_file(&inf);
            return Err(e.to_string());
        }
        drop(f);
        return match std::fs::rename(p, &dest) {
            Ok(()) => {
                log_line(&format!("trash: {} -> {} (no gio, Trash spec)", p.display(), dest.display()));
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&inf);
                Err(e.to_string())
            }
        };
    }
    Err("the Trash is full of files with this name".into())
}

/// Path= in a .trashinfo is URL-escaped (RFC 2396); "/" stays.
fn url_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------- row 52: Do Not Disturb tip

/// After a saved clip (Echo's dndTip, 1.5 s later): KDE turns Do Not Disturb on by itself while the screen is
/// shared, so "Clip saved" never pops up. Say so ONCE (native_ui.json "echo_dndtip" = Echo's localStorage key).
pub fn clip_saved(a: &AppWindow) {
    let w = a.as_weak();
    slint::Timer::single_shot(Duration::from_millis(1500), move || {
        let Some(a) = w.upgrade() else { return };
        if !crate::lib_fx::ui_get("echo_dndtip").is_null() || !linuxutil::dnd_on() {
            return;
        }
        crate::lib_fx::ui_set("echo_dndtip", serde_json::json!("1"));
        log_line("ui: Do Not Disturb tip shown");
        crate::ask(
            &a,
            "Your \"Clip saved\" popups are hidden",
            "Your desktop's Do Not Disturb is on, so Echo's popups only go to the notification history. KDE switches it on by itself while your screen is being shared (by Discord, a browser, or the Flatpak version of Echo, which records that way).\n\nTo see them: System Settings > Notifications > Do Not Disturb, and untick \"During screen sharing\".\n\nThe save sound still plays either way. This tip only shows once.",
            &[("Got it", "ok", "primary")],
            None,
            "ok",
            |_, _, _| {},
        );
    });
}

// ---------------------------------------------------------------- row 53: Flatpak first start

/// Flatpak: one desktop dialog at a time - the Welcome screen, then the shortcut dialog (hotkey_linux waits for
/// open_portals), then the screen-share dialog (= recording starts). Blocks the recorder's start thread until
/// then; returns at once outside the Flatpak. Same loop as src-tauri main.rs.
pub fn flatpak_wait() {
    if !linuxutil::in_flatpak() {
        return;
    }
    let mut said = false;
    loop {
        if !linuxutil::portals_open() && setup_done() {
            std::thread::sleep(Duration::from_millis(1500));
            linuxutil::open_portals();
        }
        if linuxutil::flatpak_may_record() {
            break;
        }
        if !said {
            said = true;
            log_line("flatpak: recording starts after the Welcome screen and the shortcut dialog");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Flatpak "Start with the computer": the Background portal writes the autostart entry (~/.config/autostart is
/// outside the sandbox). Like src-tauri: only when it changes, never a "no" on the first call; and not before the
/// Welcome screen is done (it can be a desktop dialog too).
pub fn flatpak_autostart(on: bool) {
    static LAST: std::sync::Mutex<Option<bool>> = std::sync::Mutex::new(None);
    {
        let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let prev = l.replace(on);
        if prev == Some(on) || (prev.is_none() && !on) {
            return;
        }
    }
    std::thread::spawn(move || {
        while !linuxutil::portals_open() {
            std::thread::sleep(Duration::from_millis(500));
        }
        linuxutil::portal_autostart(on);
    });
}

// ---------------------------------------------------------------- file pickers

/// File / folder picker (Echo's picker::pick_path): the desktop's portal in the Flatpak (no kdialog / zenity
/// there); otherwise KDE's kdialog, zenity, and the FileChooser portal when neither is installed.
/// `filter` = ("Videos", "*.mp4;*.mkv"). Blocking: call it on a worker thread.
pub fn pick(title: &str, filter: Option<(&str, &str)>, folder: bool) -> Option<PathBuf> {
    let some = |s: String| (!s.is_empty()).then(|| PathBuf::from(s));
    if linuxutil::in_flatpak() {
        return linuxutil::portal_pick(title, filter, folder).and_then(some);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let mut k = sys_cmd("kdialog");
    k.arg("--title").arg(title);
    if folder {
        k.arg("--getexistingdirectory").arg(&home);
    } else {
        k.arg("--getopenfilename").arg(&home);
        if let Some((n, p)) = filter {
            k.arg(format!("{n} ({})", p.replace(';', " ")));
        }
    }
    let out = match k.output() {
        Ok(o) => o,
        Err(_) => {
            let mut z = sys_cmd("zenity");
            z.args(["--file-selection", "--title", title]);
            if folder {
                z.arg("--directory");
            }
            if let Some((n, p)) = filter {
                z.arg(format!("--file-filter={n} | {}", p.replace(';', " ")));
            }
            match z.output() {
                Ok(o) => o,
                Err(_) => {
                    log_line("picker: no kdialog or zenity - using the desktop's file chooser portal");
                    return linuxutil::portal_pick(title, filter, folder).and_then(some);
                }
            }
        }
    };
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        some(s)
    } else {
        None
    }
}
