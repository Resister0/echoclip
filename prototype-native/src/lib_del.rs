//! NATIVE_GAPS row 42 (10 Nov 2026): Echo's delete dialogs for library clips (#pickdel + the card's "del").
//! - one clip also in the cloud: choiceBox [Cancel][Only from this PC][Everywhere (red)], Enter does nothing
//! - several clips: "Delete N clips?" + "N of them aren't in the view you're looking at (other folders / filters)."
//! - more than one clip: a progress box "Deleting clips…" / "i of N" while the cards disappear
use crate::library::LibModel;
use crate::{AppWindow, Play};
use slint::ComponentHandle;
use std::rc::Rc;
use std::time::Duration;

fn bin() -> &'static str {
    if cfg!(windows) {
        "Recycle Bin"
    } else {
        "Trash"
    }
}

fn sync_all(a: &AppWindow, lib: &Rc<LibModel>) {
    lib.refresh();
    crate::home_fill(a, lib);
    crate::UI_SYNC.with(|f| {
        if let Some(f) = f.borrow().as_ref() {
            f(true)
        }
    });
}

/// Echo's delete for these clips (ids = clip ids). Cloud-only picks alone go to the cloud session's
/// `cloud::delete_cloud_only` as before.
pub fn delete_ui(a: &AppWindow, lib: Rc<LibModel>, ids: Vec<usize>) {
    let ids: Vec<usize> = ids.into_iter().filter(|&i| i < lib.len() && !lib.clip(i).gone.get()).collect();
    if ids.is_empty() {
        return;
    }
    if ids.iter().all(|&i| lib.clip(i).cloud.is_some()) {
        crate::cloud::delete_cloud_only(a, ids);
        return;
    }
    let n = ids.len();
    let n_cloud = ids.iter().filter(|&&i| lib.in_cloud(i) || lib.clip(i).cloud.is_some()).count();
    let n_only = ids.iter().filter(|&&i| lib.clip(i).cloud.is_some()).count();
    if n == 1 {
        let c = lib.clip(ids[0]);
        let ids2 = ids.clone();
        if n_cloud > 0 {
            let msg = format!("\"{}\" is on this PC and backed up in your cloud.", c.shown);
            crate::ask(a, "Delete clip?", &msg, &[("Cancel", "", ""), ("Only from this PC", "pc", ""), ("Everywhere", "all", "delb")], None, "", move |a, v, _| match v.as_str() {
                "pc" => run(a, lib, ids2, true),
                "all" => crate::cloud::delete_everywhere(a, ids2),
                _ => {}
            });
        } else {
            let msg = format!("\"{}\" will be moved to the {}.", c.shown, bin());
            crate::confirm_box(a, "Delete clip?", &msg, "Delete", move |a| run(a, lib, ids2, false));
        }
        return;
    }
    // picked clips can be in other folders / hidden by a filter: say so first (Echo's hidNote)
    let hidden = ids.iter().filter(|&&i| lib.view_row_of(i).is_none()).count();
    let hid_note = if hidden > 0 {
        format!("{hidden} of them {} in the view you're looking at (other folders / filters).", if hidden == 1 { "isn't" } else { "aren't" })
    } else {
        String::new()
    };
    let title = format!("Delete {n} clips?");
    if n_cloud > 0 {
        let also = if n_cloud == n { "They're all".to_string() } else { format!("{n_cloud} of them are") };
        let only = if n_only > 0 { format!(" {n_only} only live in the cloud - \"Only from this PC\" leaves those alone.") } else { String::new() };
        let msg = format!("{}{}{also} also in your cloud.{only}", hid_note, if hid_note.is_empty() { "" } else { " " });
        crate::ask(a, &title, &msg, &[("Cancel", "", ""), ("Only from this PC", "pc", ""), ("Everywhere", "all", "delb")], None, "", move |a, v, _| match v.as_str() {
            "pc" => run(a, lib, ids, true),
            "all" => crate::cloud::delete_everywhere(a, ids),
            _ => {}
        });
    } else {
        let msg = format!("They will be moved to the {}.{}{hid_note}", bin(), if hid_note.is_empty() { "" } else { " " });
        crate::confirm_box(a, &title, &msg, "Delete", move |a| run(a, lib, ids, false));
    }
}

/// "Only from this PC" / plain delete: the player lets go first, then one clip per tick so the box counts up and
/// the cards disappear as it goes (Echo refreshes every 5). Cloud-only clips are left alone (Echo: `if (!c.cloud)`).
fn run(a: &AppWindow, lib: Rc<LibModel>, ids: Vec<usize>, cloud: bool) {
    if a.get_player_open() {
        a.global::<Play>().invoke_close(); // mpv lets go of the file first
    }
    crate::hover_vid::stop(a);
    let ids: Vec<usize> = ids.into_iter().filter(|&i| lib.clip(i).cloud.is_none()).collect();
    let total = ids.len();
    if total == 0 {
        return;
    }
    let bx = (total > 1).then(|| {
        let id = crate::prog_box("Deleting clips…");
        crate::prog_set(id, None, Some(&format!("0 of {total}")), Some(0.0));
        id
    });
    let w = a.as_weak();
    let before = crate::undo::before_delete(&lib, &ids); // 2.0.2: what Undo puts back
    // r2-threads (L5): the Recycle Bin / Trash runs on a worker, a clip at a time; the box and cards follow here
    slint::Timer::single_shot(Duration::from_millis(250), move || {
        let (l2, l3) = (lib.clone(), lib.clone());
        l3.delete_clips_bg(
            &ids,
            move |i, total| {
                if let Some(id) = bx {
                    crate::prog_set(id, None, Some(&format!("{i} of {total}")), Some(i as f32 / total as f32 * 100.0));
                }
                if i % 5 == 0 {
                    l2.refresh();
                }
            },
            move |_done, err| {
                if let Some(id) = bx {
                    crate::prog_set(id, None, Some(&format!("{total} of {total}")), Some(100.0));
                    crate::prog_done(id, 0); // 2.0.2: the result note (with Undo) takes its place
                }
                let Some(a) = w.upgrade() else { return };
                lib.clear_picks();
                sync_all(&a, &lib);
                // 2.0.2: "Clip deleted · Undo" (repeats add up); an error stays on its own red note
                if let Some(e) = err {
                    crate::notes::say(crate::notes::Kind::Err, e);
                }
                let went: Vec<_> = before.into_iter().filter(|g| !g.path.exists()).collect();
                crate::undo::deleted(&a, &lib, went, cloud);
            },
        );
    });
}
