//! Port leftovers L2 (10 Nov): hand the keyboard back after a rename, overlay, dropdown or dialog closes.
//! Echo's key handlers sat on the document, so Ctrl+F / Ctrl+A / Esc / Space / Del kept working after any of those.
//! In Slint the focus stays on the hidden or destroyed item and every key is dropped until the next click.
use crate::{AppWindow, Dlg, Ed, Focus, Su, Tidy, Tour};
use slint::ComponentHandle;

/// focus whatever is on top now: dialog, tour, editor, Tidy, player, Settings, else the library / Home / Screenshots
pub fn back(a: &AppWindow) {
    if a.global::<Dlg>().get_open() {
        a.invoke_focus_dialog();
    } else if a.global::<Tour>().get_step() >= 0 {
        a.invoke_focus_tour();
    } else if a.global::<Su>().get_open() {
        // the setup wizard has no keys of its own (only hotkey capture, which focuses itself): don't wake the
        // library's keys behind it
    } else if a.global::<Ed>().get_open() {
        // the editor keeps the player "open" but hidden: check it first (L2 "Leave editor?" → Cancel)
        a.invoke_focus_editor();
    } else if a.global::<Tidy>().get_open() {
        a.invoke_focus_tidy();
    } else if a.get_player_open() {
        a.invoke_focus_player();
    } else if a.get_view() == "settings" {
        a.invoke_focus_settings();
    } else {
        a.invoke_focus_library();
    }
}

/// `Focus.back()` from the .slint close paths (rename boxes, Tidy, screenshot viewer, Settings dropdown, filter search)
pub fn setup(app: &AppWindow) {
    let w = app.as_weak();
    app.global::<Focus>().on_back(move || {
        if let Some(a) = w.upgrade() {
            back(&a);
        }
    });
}
