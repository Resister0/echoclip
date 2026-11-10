//! gaps-library row 29 (10 Nov 2026): the sidebar folder's right-click "Merge into <folder>" and "Merge into the
//! main folder" (Echo's mergeFolder: asks "Merge folders?" first).
use crate::library::LibModel;
use crate::AppWindow;
use slint::ComponentHandle;
use std::rc::Rc;

/// `to` = the folder to merge into ("" = the main folder)
pub fn merge_ui(a: &AppWindow, lib: Rc<LibModel>, g: String, to: String, sync: Rc<dyn Fn(bool)>) {
    let dest = if to.is_empty() { "the main folder".to_string() } else { format!("\"{to}\"") };
    let msg = format!("All clips in \"{g}\" move into {dest}. New {g} clips will go there too.");
    crate::confirm_box(a, "Merge folders?", &msg, "Merge", move |a| {
        // a hover preview / the player keeps a file open: Windows then refuses the move
        crate::hover_vid::stop(a);
        // r2-threads (L13): the files move on a worker
        let (w, l2, g2, to2) = (a.as_weak(), lib.clone(), g.clone(), to.clone());
        let done = move |r: Result<(), String>| {
            let Some(a) = w.upgrade() else { return };
            let (a, lib, g, to) = (&a, l2, g2, to2);
            match r {
                Ok(_) => crate::toast(a, format!("Merged \"{g}\" into {}", if to.is_empty() { "the main folder" } else { to.as_str() })),
                Err(e) => crate::toast(a, e),
            }
            lib.refresh();
            a.set_filter(lib.nav.borrow().as_str().into());
            sync(true);
        };
        if to.is_empty() {
            lib.merge_into_main_bg(&g, done);
        } else {
            lib.move_folder_bg(&g, &to, move |r| done(r.map(|_| ())));
        }
    });
}
