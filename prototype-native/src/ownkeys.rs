//! Windows: Echo's hotkeys (save keys, screenshot, bookmark, session, mic mute, Show / hide) while Echo's OWN
//! window has the keyboard. 13 Nov 2026 (2.0.9 test): pressed inside Echo, none of them did anything - the keyboard
//! hook (echo_core hotkey.rs) never fired while Echo was the foreground window, only outside it (the old Tauri Echo
//! had the same gap and sent such keys from its window: b54702b, hotkey::ui_press; the native port lost that path).
//! Now Echo's window sends every key press here too -> hotkey::ui_press, which finds the hotkey and runs it like the
//! hook does. If the hook sees the same press as well, ui_press's first_fire (0.4 s) keeps it to one.
use slint::winit_030::winit;

thread_local! {
    /// modifiers held now (winit's ModifiersChanged): ctrl, alt, shift, super
    static MODS: std::cell::Cell<(bool, bool, bool, bool)> = const { std::cell::Cell::new((false, false, false, false)) };
}

/// main.rs's winit hook: every window event (always lets it through)
pub fn winit_event(ev: &winit::event::WindowEvent) {
    use winit::event::{ElementState, WindowEvent};
    use winit::keyboard::PhysicalKey;
    match ev {
        WindowEvent::ModifiersChanged(m) => {
            let s = m.state();
            MODS.with(|x| x.set((s.control_key(), s.alt_key(), s.shift_key(), s.super_key())));
        }
        WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && !event.repeat => {
            let PhysicalKey::Code(code) = event.physical_key else { return };
            let (ctrl, alt, shift, sup) = MODS.with(|x| x.get());
            if let Some(keys) = combo(ctrl, alt, shift, sup, &format!("{code:?}")) {
                #[cfg(windows)]
                echo_core::hotkey::ui_press(&keys);
                #[cfg(not(windows))]
                let _ = keys;
            }
        }
        WindowEvent::Focused(false) => MODS.with(|x| x.set((false, false, false, false))),
        _ => {}
    }
}

/// winit key + held modifiers -> the combo text Settings saves ("Alt+F12", "Control+Shift+KeyC"; settings.rs
/// combo). None for a modifier on its own.
fn combo(ctrl: bool, alt: bool, shift: bool, sup: bool, code: &str) -> Option<String> {
    if code.starts_with("Control") || code.starts_with("Alt") || code.starts_with("Shift") || code.starts_with("Super") || code.starts_with("Meta") {
        return None;
    }
    let mut parts: Vec<&str> = vec![];
    if ctrl {
        parts.push("Control");
    }
    if alt {
        parts.push("Alt");
    }
    if shift {
        parts.push("Shift");
    }
    if sup {
        parts.push("Super");
    }
    parts.push(code);
    Some(parts.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode;

    #[test]
    fn same_text_as_settings() {
        // winit's key names are the ones Settings saves (KeyboardEvent.code)
        let n = |c: KeyCode| format!("{c:?}");
        assert_eq!(combo(false, true, false, false, &n(KeyCode::F12)).as_deref(), Some("Alt+F12"));
        assert_eq!(combo(false, true, false, false, &n(KeyCode::F10)).as_deref(), Some("Alt+F10"));
        assert_eq!(combo(false, false, true, false, &n(KeyCode::F11)).as_deref(), Some("Shift+F11"));
        assert_eq!(combo(true, false, true, false, &n(KeyCode::KeyC)).as_deref(), Some("Control+Shift+KeyC"));
        assert_eq!(combo(false, false, false, false, &n(KeyCode::Digit1)).as_deref(), Some("Digit1"));
        // and Settings' own text for the same keys (Slint key event -> combo)
        let f12: slint::SharedString = slint::platform::Key::F12.into();
        assert_eq!(crate::settings::combo(&f12, false, true, false, false, false), combo(false, true, false, false, &n(KeyCode::F12)));
        assert_eq!(crate::settings::combo("c", true, false, true, false, false), combo(true, false, true, false, &n(KeyCode::KeyC)));
    }

    #[test]
    fn modifier_alone_is_no_key() {
        for c in [KeyCode::AltLeft, KeyCode::ControlRight, KeyCode::ShiftLeft, KeyCode::SuperLeft] {
            assert_eq!(combo(true, true, true, true, &format!("{c:?}")), None);
        }
    }

    #[cfg(windows)]
    #[test]
    fn hotkey_finds_it() {
        // the user's keys as saved in Settings; a press in Echo's window must find each one (Show / hide included)
        let show = crate::showkey::SHOW;
        assert!(echo_core::hotkey::set(&[("Alt+F12".into(), show), ("Alt+F10".into(), 30), ("Shift+F11".into(), 60), ("Control+Shift+KeyC".into(), 90)]).is_empty());
        let f = |c, a, s, k| echo_core::hotkey::find(&combo(c, a, s, false, k).unwrap());
        assert_eq!(f(false, true, false, "F12"), Some(show));
        assert_eq!(f(false, true, false, "F10"), Some(30));
        assert_eq!(f(false, false, true, "F11"), Some(60));
        assert_eq!(f(true, false, true, "KeyC"), Some(90));
        // other modifiers / keys: nothing
        assert_eq!(f(false, false, false, "F12"), None);
        assert_eq!(f(true, true, false, "F12"), None);
        assert_eq!(f(false, true, false, "F9"), None);
        echo_core::hotkey::set(&[]);
    }
}
