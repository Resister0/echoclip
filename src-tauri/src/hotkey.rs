//! Medal-style global hotkeys: a low-level keyboard hook (WH_KEYBOARD_LL) sees every key
//! before games do, fires on the first press, works with F12, and never swallows keys.

use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

struct Combo { ctrl: bool, alt: bool, shift: bool, win: bool, vk: usize, seconds: u32 }

static COMBOS: Mutex<Vec<Combo>> = Mutex::new(Vec::new());
static DOWN: Mutex<[bool; 256]> = Mutex::new([false; 256]);
/// last key-down time per key (ms tick), to tell autorepeat from a fresh press even if a key-up was missed
static LAST: Mutex<[u32; 256]> = Mutex::new([0; 256]);
/// last time a save fired (ms tick) - guards against double triggers
static FIRED: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// true (and remembers it) unless something fired in the last 0.4 s - the hook and Echo's own window can both
/// see the same key press
fn first_fire() -> bool {
    let mut f = FIRED.lock().unwrap_or_else(|e| e.into_inner());
    if f.map_or(false, |t| t.elapsed() < std::time::Duration::from_millis(400)) { return false; }
    *f = Some(std::time::Instant::now());
    true
}
/// A save key pressed while Echo's own window is in front (sent by the window, keys as in the settings).
pub fn ui_press(keys: &str) {
    if paused() { return; }
    let want = parse(keys, 0);
    let secs = COMBOS.lock().ok().and_then(|c| c.iter()
        .find(|c| c.vk == want.vk && c.ctrl == want.ctrl && c.alt == want.alt && c.shift == want.shift && c.win == want.win)
        .map(|c| c.seconds));
    if let Some(secs) = secs {
        if first_fire() {
            crate::recorder::log_line(&format!("hotkey: {keys} pressed in Echo's window"));
            std::thread::spawn(move || if let Some(cb) = CB.get() { cb(secs) });
        }
    }
}
fn parse(keys: &str, seconds: u32) -> Combo {
    let mut c = Combo { ctrl: false, alt: false, shift: false, win: false, vk: 0, seconds };
    for part in keys.split('+') {
        match part {
            "Control" | "Ctrl" => c.ctrl = true,
            "Alt" => c.alt = true,
            "Shift" => c.shift = true,
            "Super" | "Meta" => c.win = true,
            k => c.vk = vk_of(k).unwrap_or(0),
        }
    }
    c
}
/// push-to-talk key (virtual-key code, 0 = off)
static PTT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static CB: OnceLock<Box<dyn Fn(u32) + Send + Sync>> = OnceLock::new();

/// KeyboardEvent.code name -> virtual-key code.
fn vk_of(code: &str) -> Option<usize> {
    let c = code.as_bytes();
    Some(match code {
        _ if code.starts_with("Key") && c.len() == 4 => c[3] as usize,
        _ if code.starts_with("Digit") && c.len() == 6 => c[5] as usize,
        _ if code.starts_with("Numpad") && c.len() == 7 && c[6].is_ascii_digit() => 0x60 + (c[6] - b'0') as usize,
        _ if code.starts_with('F') && code[1..].parse::<usize>().map_or(false, |n| (1..=24).contains(&n)) =>
            0x6F + code[1..].parse::<usize>().unwrap(),
        "Space" => 0x20, "Insert" => 0x2D, "Delete" => 0x2E, "Home" => 0x24, "End" => 0x23,
        "PageUp" => 0x21, "PageDown" => 0x22, "PrintScreen" => 0x2C, "Pause" => 0x13, "ScrollLock" => 0x91,
        "ArrowLeft" => 0x25, "ArrowUp" => 0x26, "ArrowRight" => 0x27, "ArrowDown" => 0x28,
        "Backquote" => 0xC0, "Minus" => 0xBD, "Equal" => 0xBB, "BracketLeft" => 0xDB, "BracketRight" => 0xDD,
        "Backslash" => 0xDC, "Semicolon" => 0xBA, "Quote" => 0xDE, "Comma" => 0xBC, "Period" => 0xBE,
        "Slash" => 0xBF, "Tab" => 0x09, "Enter" => 0x0D, "Backspace" => 0x08,
        "CapsLock" => 0x14, "ShiftLeft" => 0xA0, "ShiftRight" => 0xA1, "ControlLeft" => 0xA2, "ControlRight" => 0xA3,
        "AltLeft" => 0xA4, "AltRight" => 0xA5, "NumpadDecimal" => 0x6E, "NumpadEnter" => 0x0D,
        "NumpadAdd" => 0x6B, "NumpadSubtract" => 0x6D, "NumpadMultiply" => 0x6A, "NumpadDivide" => 0x6F,
        _ => return None,
    })
}

/// Push-to-talk key (a single KeyboardEvent.code name, "" = off).
pub fn set_ptt(code: &str) {
    PTT.store(vk_of(code).unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
}

/// Replace the active hotkeys. Returns the ones that could not be parsed.
pub fn set(list: &[(String, u32)]) -> Vec<String> {
    let mut bad = Vec::new();
    let mut v = Vec::new();
    for (keys, seconds) in list {
        let c = parse(keys, *seconds);
        if c.vk == 0 { bad.push(keys.clone()) } else { v.push(c) }
    }
    *COMBOS.lock().unwrap() = v;
    bad
}

unsafe extern "system" fn hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        let k = &*(l.0 as *const KBDLLHOOKSTRUCT);
        let vk = (k.vkCode & 0xFF) as usize;
        let msg = w.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        let p = PTT.load(std::sync::atomic::Ordering::Relaxed);
        // the hook reports left/right modifier codes; generic Shift/Ctrl/Alt match either side
        if p != 0 && (vk == p || matches!((p, vk), (0x10, 0xA0 | 0xA1) | (0x11, 0xA2 | 0xA3) | (0x12, 0xA4 | 0xA5))) {
            if down { crate::recorder::PTT_HELD.store(true, std::sync::atomic::Ordering::Relaxed); }
            if up { crate::recorder::PTT_HELD.store(false, std::sync::atomic::Ordering::Relaxed); }
        }
        if let (Ok(mut d), Ok(mut last)) = (DOWN.try_lock(), LAST.try_lock()) {
            let now = k.time;
            // autorepeat = key already down and seen again within ~1s; anything older means we missed a key-up
            let repeat = d[vk] && now.wrapping_sub(last[vk]) < 1100;
            if down { d[vk] = true; last[vk] = now; }
            if up { d[vk] = false; }
            if down && !repeat {
                // modifiers straight from Windows (tracked state can go stale when keys are released in another app)
                use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};
                let held = |v: u16| (GetAsyncKeyState(v as i32) as u16 & 0x8000) != 0;
                let ctrl = held(VK_CONTROL.0);
                let alt = held(VK_MENU.0) || msg == WM_SYSKEYDOWN && k.flags.0 & 0x20 != 0;
                let shift = held(VK_SHIFT.0);
                let win = held(VK_LWIN.0) || held(VK_RWIN.0);
                if let Ok(c) = COMBOS.try_lock() {
                    if let Some(m) = c.iter().find(|c| c.vk == vk && c.ctrl == ctrl && c.alt == alt && c.shift == shift && c.win == win) {
                        let secs = m.seconds;
                        if paused() { return CallNextHookEx(None, code, w, l); }
                        if first_fire() {
                            std::thread::spawn(move || if let Some(cb) = CB.get() { cb(secs) });
                        }
                    }
                }
            }
        }
    }
    CallNextHookEx(None, code, w, l)
}

/// Install the hook on its own thread (needs a message loop). Call once.
pub fn start(cb: Box<dyn Fn(u32) + Send + Sync>) {
    let _ = CB.set(cb);
    std::thread::spawn(|| unsafe {
        if SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0).is_err() {
            crate::recorder::log_line("hotkey: failed to install keyboard hook");
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
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
