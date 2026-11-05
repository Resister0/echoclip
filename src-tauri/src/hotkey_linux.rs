//! Linux global hotkeys (Wayland has no key hooks for apps).
//! 1) evdev: read /dev/input/event* directly (user must be in the `input` group) - keys are set in Echo.
//! 2) fallback: XDG GlobalShortcuts portal (KDE asks once to confirm / lets the user change the keys).
//! Same API as hotkey.rs on Windows: set(), set_ptt(), start().

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Combo { ctrl: bool, alt: bool, shift: bool, meta: bool, key: u16, seconds: u32, keys: String }

static COMBOS: Mutex<Vec<Combo>> = Mutex::new(Vec::new());
static PTT: AtomicU16 = AtomicU16::new(0);
static PTT_CODE: Mutex<String> = Mutex::new(String::new());
static CB: OnceLock<Box<dyn Fn(u32) + Send + Sync>> = OnceLock::new();
static FIRED: Mutex<Option<Instant>> = Mutex::new(None);
static PORTAL: AtomicBool = AtomicBool::new(false);
static PORTAL_DIRTY: AtomicBool = AtomicBool::new(false);

/// KeyboardEvent.code -> Linux evdev key code (linux/input-event-codes.h)
fn key_of(code: &str) -> Option<u16> {
    let letters = "QWERTYUIOPASDFGHJKLZXCVBNM";
    let lcodes = [16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 30, 31, 32, 33, 34, 35, 36, 37, 38, 44, 45, 46, 47, 48, 49, 50];
    if let Some(l) = code.strip_prefix("Key") {
        let i = letters.find(l)?;
        return if l.len() == 1 { Some(lcodes[i]) } else { None };
    }
    if let Some(d) = code.strip_prefix("Digit") {
        let n: u16 = d.parse().ok()?;
        return Some(if n == 0 { 11 } else { n + 1 });
    }
    if let Some(f) = code.strip_prefix('F') {
        if let Ok(n) = f.parse::<u16>() {
            return match n { 1..=10 => Some(58 + n), 11 => Some(87), 12 => Some(88), 13..=24 => Some(170 + n), _ => None };
        }
    }
    if let Some(d) = code.strip_prefix("Numpad") {
        if let Ok(n) = d.parse::<usize>() {
            return [82u16, 79, 80, 81, 75, 76, 77, 71, 72, 73].get(n).copied();
        }
    }
    Some(match code {
        "Escape" => 1, "Minus" => 12, "Equal" => 13, "Backspace" => 14, "Tab" => 15,
        "BracketLeft" => 26, "BracketRight" => 27, "Enter" => 28, "ControlLeft" => 29, "Semicolon" => 39,
        "Quote" => 40, "Backquote" => 41, "ShiftLeft" => 42, "Backslash" => 43, "Comma" => 51, "Period" => 52,
        "Slash" => 53, "ShiftRight" => 54, "NumpadMultiply" => 55, "AltLeft" => 56, "Space" => 57, "CapsLock" => 58,
        "ScrollLock" => 70, "NumpadSubtract" => 74, "NumpadAdd" => 78, "NumpadDecimal" => 83, "NumpadEnter" => 96,
        "ControlRight" => 97, "NumpadDivide" => 98, "PrintScreen" => 99, "AltRight" => 100, "Home" => 102,
        "ArrowUp" => 103, "PageUp" => 104, "ArrowLeft" => 105, "ArrowRight" => 106, "End" => 107,
        "ArrowDown" => 108, "PageDown" => 109, "Insert" => 110, "Delete" => 111, "Pause" => 119,
        "MetaLeft" => 125, "MetaRight" => 126,
        // generic modifiers as a push-to-talk key
        "Shift" => 42, "Control" => 29, "Alt" => 56,
        _ => return None,
    })
}

/// KeyboardEvent.code -> xkb key name for the portal's preferred trigger
fn xkb_of(code: &str) -> Option<String> {
    if let Some(l) = code.strip_prefix("Key") { return Some(l.to_lowercase()); }
    if let Some(d) = code.strip_prefix("Digit") { return Some(d.into()); }
    if code.starts_with('F') && code[1..].parse::<u16>().is_ok() { return Some(code.into()); }
    if let Some(d) = code.strip_prefix("Numpad") { if d.parse::<u8>().is_ok() { return Some(format!("KP_{d}")); } }
    Some(match code {
        "Space" => "space", "Insert" => "Insert", "Delete" => "Delete", "Home" => "Home", "End" => "End",
        "PageUp" => "Prior", "PageDown" => "Next", "PrintScreen" => "Print", "Pause" => "Pause",
        "ScrollLock" => "Scroll_Lock", "ArrowLeft" => "Left", "ArrowRight" => "Right", "ArrowUp" => "Up",
        "ArrowDown" => "Down", "Tab" => "Tab", "Enter" => "Return", "Backquote" => "grave", "Minus" => "minus",
        "Equal" => "equal", "CapsLock" => "Caps_Lock",
        _ => return None,
    }.into())
}

fn trigger(keys: &str) -> Option<String> {
    let mut mods = Vec::new();
    let mut key = None;
    for p in keys.split('+') {
        match p {
            "Control" | "Ctrl" => mods.push("CTRL"),
            "Alt" => mods.push("ALT"),
            "Shift" => mods.push("SHIFT"),
            "Super" | "Meta" => mods.push("LOGO"),
            k => key = xkb_of(k),
        }
    }
    let k = key?;
    let mut s = mods.join("+");
    if !s.is_empty() { s.push('+'); }
    Some(s + &k)
}

pub fn set_ptt(code: &str) {
    PTT.store(key_of(code).unwrap_or(0), Ordering::Relaxed);
    let mut p = PTT_CODE.lock().unwrap();
    if *p != code { *p = code.to_string(); PORTAL_DIRTY.store(true, Ordering::Relaxed); }
}

/// Replace the active hotkeys. Returns the ones that could not be parsed.
pub fn set(list: &[(String, u32)]) -> Vec<String> {
    let mut bad = Vec::new();
    let mut v = Vec::new();
    for (keys, seconds) in list {
        let mut c = Combo { ctrl: false, alt: false, shift: false, meta: false, key: 0, seconds: *seconds, keys: keys.clone() };
        for part in keys.split('+') {
            match part {
                "Control" | "Ctrl" => c.ctrl = true,
                "Alt" => c.alt = true,
                "Shift" => c.shift = true,
                "Super" | "Meta" => c.meta = true,
                k => c.key = key_of(k).unwrap_or(0),
            }
        }
        if c.key == 0 { bad.push(keys.clone()) } else { v.push(c) }
    }
    let mut cur = COMBOS.lock().unwrap();
    let changed = cur.len() != v.len() || cur.iter().zip(&v).any(|(a, b)| a.keys != b.keys || a.seconds != b.seconds);
    *cur = v;
    if changed { PORTAL_DIRTY.store(true, Ordering::Relaxed); }
    bad
}

fn fire(secs: u32) {
    if paused() { return; }
    let mut f = FIRED.lock().unwrap_or_else(|e| e.into_inner());
    if f.map_or(true, |t| t.elapsed() > Duration::from_millis(400)) {
        *f = Some(Instant::now());
        std::thread::spawn(move || if let Some(cb) = CB.get() { cb(secs) });
    }
}

pub fn start(cb: Box<dyn Fn(u32) + Send + Sync>) {
    let _ = CB.set(cb);
    let readable = std::fs::read_dir("/dev/input").ok().map_or(0, |rd| rd.flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
        .filter(|e| std::fs::File::open(e.path()).is_ok()).count());
    if readable > 0 {
        crate::recorder::log_line(&format!("hotkey: evdev ({readable} input devices)"));
        std::thread::spawn(evdev_loop);
    } else {
        crate::recorder::log_line("hotkey: no access to /dev/input (not in the 'input' group) - using the KDE shortcuts portal");
        PORTAL.store(true, Ordering::Relaxed);
        std::thread::spawn(|| loop {
            if let Err(e) = portal_loop() { crate::recorder::log_line(&format!("hotkey: portal: {e}")); }
            std::thread::sleep(Duration::from_secs(5));
        });
    }
}

// ---------------- evdev ----------------

/// shared key state across all keyboards (modifier on one device, key on another)
static DOWN: Mutex<[bool; 768]> = Mutex::new([false; 768]);

fn evdev_loop() {
    let mut open: HashMap<std::path::PathBuf, ()> = HashMap::new();
    loop {
        if let Ok(rd) = std::fs::read_dir("/dev/input") {
            for e in rd.flatten() {
                let p = e.path();
                if !e.file_name().to_string_lossy().starts_with("event") || open.contains_key(&p) { continue; }
                if let Ok(f) = std::fs::File::open(&p) {
                    open.insert(p.clone(), ());
                    std::thread::spawn(move || read_dev(f));
                }
            }
        }
        // new devices (a keyboard plugged in later); dead ones are retried
        std::thread::sleep(Duration::from_secs(3));
        open.retain(|p, _| p.exists());
    }
}

fn read_dev(mut f: std::fs::File) {
    use std::io::Read;
    let mut b = [0u8; 24 * 64]; // struct input_event = 24 bytes on 64-bit
    loop {
        let n = match f.read(&mut b) { Ok(0) | Err(_) => return, Ok(n) => n };
        for ev in b[..n].chunks_exact(24) {
            let typ = u16::from_ne_bytes([ev[16], ev[17]]);
            let code = u16::from_ne_bytes([ev[18], ev[19]]);
            let val = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
            if typ != 1 || code as usize >= 768 { continue; } // EV_KEY only
            key_event(code, val);
        }
    }
}

fn key_event(code: u16, val: i32) {
    // val: 1 down, 0 up, 2 autorepeat
    let p = PTT.load(Ordering::Relaxed);
    let pair = |a: u16, b: u16| (code == a || code == b) && (p == a || p == b);
    if p != 0 && (code == p || pair(42, 54) || pair(29, 97) || pair(56, 100)) {
        if val == 1 { crate::recorder::PTT_HELD.store(true, Ordering::Relaxed); }
        if val == 0 { crate::recorder::PTT_HELD.store(false, Ordering::Relaxed); }
    }
    let Ok(mut d) = DOWN.lock() else { return };
    let was = d[code as usize];
    if val == 1 { d[code as usize] = true; }
    if val == 0 { d[code as usize] = false; }
    if val != 1 || was { return; }
    let ctrl = d[29] || d[97];
    let alt = d[56] || d[100];
    let shift = d[42] || d[54];
    let meta = d[125] || d[126];
    drop(d);
    if let Ok(c) = COMBOS.lock() {
        if let Some(m) = c.iter().find(|c| c.key == code && c.ctrl == ctrl && c.alt == alt && c.shift == shift && c.meta == meta) {
            fire(m.seconds);
        }
    }
}

// ---------------- XDG GlobalShortcuts portal ----------------

fn portal_loop() -> Result<(), String> {
    use dbus::arg::{PropMap, RefArg, Variant};
    use dbus::blocking::Connection;
    use dbus::message::MatchRule;
    // Flatpak first start: no shortcut dialog on top of the Welcome screen
    while !crate::linuxutil::portals_open() { std::thread::sleep(Duration::from_millis(500)); }
    let conn = Connection::new_session().map_err(|e| e.to_string())?;
    let sender = conn.unique_name().trim_start_matches(':').replace('.', "_");
    let proxy = conn.with_proxy("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", Duration::from_secs(30));
    let iface = "org.freedesktop.portal.GlobalShortcuts";
    // Activated / Deactivated signals carry (session, shortcut id, timestamp, options)
    let ids: std::sync::Arc<Mutex<HashMap<String, u32>>> = Default::default();
    for (sig, down) in [("Activated", true), ("Deactivated", false)] {
        let ids = ids.clone();
        conn.add_match(MatchRule::new_signal(iface, sig), move |_: (), _, msg| {
            if let (Some(_), Some(id)) = msg.get2::<dbus::Path, String>() {
                if id == "ptt" {
                    crate::recorder::PTT_HELD.store(down, Ordering::Relaxed);
                } else if down {
                    if let Some(s) = ids.lock().unwrap().get(&id) { fire(*s); }
                }
            }
            true
        }).map_err(|e| e.to_string())?;
    }
    // the answer to our BindShortcuts dialog (OK or Cancel): the screen-share dialog may come next
    let me = format!("echo{}_", std::process::id());
    conn.add_match(MatchRule::new_signal("org.freedesktop.portal.Request", "Response"), move |_: (), _, msg| {
        if msg.path().map_or(false, |p| { let p = p.to_string(); p.contains(&me) && p.ends_with('b') }) {
            crate::linuxutil::set_shortcuts_settled();
        }
        true
    }).map_err(|e| e.to_string())?;
    let mut bound_at: Option<Instant> = None;
    let mut n = 0u32;
    PORTAL_DIRTY.store(true, Ordering::Relaxed);
    let mut session: Option<dbus::Path<'static>> = None;
    loop {
        if PORTAL_DIRTY.swap(false, Ordering::Relaxed) {
            // a session's shortcuts can only be bound once: close it and make a new one
            if let Some(s) = session.take() {
                let sp = conn.with_proxy("org.freedesktop.portal.Desktop", s, Duration::from_secs(5));
                let _: Result<(), _> = sp.method_call("org.freedesktop.portal.Session", "Close", ());
            }
            n += 1;
            let tok = format!("echo{}_{n}", std::process::id());
            let mut opt = PropMap::new();
            opt.insert("handle_token".into(), Variant(Box::new(format!("{tok}r")) as Box<dyn RefArg>));
            opt.insert("session_handle_token".into(), Variant(Box::new(tok.clone()) as Box<dyn RefArg>));
            let _: (dbus::Path,) = proxy.method_call(iface, "CreateSession", (opt,)).map_err(|e| e.to_string())?;
            let sp = dbus::Path::from(format!("/org/freedesktop/portal/desktop/session/{sender}/{tok}"));
            std::thread::sleep(Duration::from_millis(300));
            let mut list: Vec<(String, PropMap)> = Vec::new();
            let mut map = HashMap::new();
            for (i, c) in COMBOS.lock().unwrap().iter().enumerate() {
                let id = format!("k{i}");
                let what = match c.seconds { 0 => "Screenshot".to_string(), s if s == u32::MAX => "Bookmark".into(),
                    s if s > 100_000 => "Echo action".into(), s => format!("Save last {s} s") };
                let mut m = PropMap::new();
                m.insert("description".into(), Variant(Box::new(format!("Echo: {what}")) as Box<dyn RefArg>));
                if let Some(t) = trigger(&c.keys) { m.insert("preferred_trigger".into(), Variant(Box::new(t) as Box<dyn RefArg>)); }
                map.insert(id.clone(), c.seconds);
                list.push((id, m));
            }
            let ptt = PTT_CODE.lock().unwrap().clone();
            if !ptt.is_empty() {
                let mut m = PropMap::new();
                m.insert("description".into(), Variant(Box::new("Echo: push to talk".to_string()) as Box<dyn RefArg>));
                if let Some(t) = trigger(&ptt) { m.insert("preferred_trigger".into(), Variant(Box::new(t) as Box<dyn RefArg>)); }
                list.push(("ptt".into(), m));
            }
            *ids.lock().unwrap() = map;
            let mut opt = PropMap::new();
            opt.insert("handle_token".into(), Variant(Box::new(format!("{tok}b")) as Box<dyn RefArg>));
            let _: (dbus::Path,) = proxy.method_call(iface, "BindShortcuts", (sp.clone(), list, "", opt)).map_err(|e| e.to_string())?;
            crate::recorder::log_line("hotkey: shortcuts sent to the KDE portal (confirm them in the KDE dialog)");
            session = Some(sp);
            bound_at.get_or_insert_with(Instant::now);
        }
        // no answer (dialog ignored / portal without one): don't hold recording back forever
        if bound_at.is_some_and(|t| t.elapsed() > Duration::from_secs(90)) { crate::linuxutil::set_shortcuts_settled(); }
        conn.process(Duration::from_millis(500)).map_err(|e| e.to_string())?;
    }
}

#[allow(dead_code)]
pub fn using_portal() -> bool {
    PORTAL.load(Ordering::Relaxed)
}

/// While Echo's settings wait for a new key combo, hotkeys must not fire (else picking an
/// existing save key saves a clip). Auto-expires after 20 s in case the UI never says "done".
static PAUSED_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
pub fn pause(on: bool) {
    *PAUSED_AT.lock().unwrap_or_else(|e| e.into_inner()) = on.then(std::time::Instant::now);
}
fn paused() -> bool {
    PAUSED_AT.lock().map_or(false, |p| p.map_or(false, |t| t.elapsed() < std::time::Duration::from_secs(20)))
}
