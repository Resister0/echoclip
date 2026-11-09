//! Chat D - card actions that are real now: drag cards onto sidebar folders, inline rename (click the name / F2),
//! the card menu's "Move to" (Echo's cardDrag / moveClips / rename_clip).
use crate::library::{self, LibModel};
use crate::{AppWindow, Flip, Lib, Play};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use std::time::Duration;

/// the clips a card action works on: the whole selection when the card is part of it, else just that card
fn targets(lib: &LibModel, idx: usize) -> Vec<usize> {
    let p = lib.picked.borrow();
    if p.contains(&idx) && p.len() > 1 {
        let mut v: Vec<usize> = p.iter().cloned().collect();
        v.sort();
        v
    } else {
        vec![idx]
    }
}

/// the card menu's "Move to" list (sidebar order). Called whenever the sidebar is rebuilt.
pub fn fill_folders(a: &AppWindow, lib: &LibModel) {
    let v: Vec<SharedString> = lib.move_targets().into_iter().map(|(f, _)| SharedString::from(f)).collect();
    let l = a.global::<Lib>();
    let cur = l.get_folders();
    use slint::Model;
    if cur.row_count() == v.len() && (0..v.len()).all(|i| cur.row_data(i).as_ref() == Some(&v[i])) {
        return;
    }
    l.set_folders(ModelRc::new(VecModel::from(v)));
}

/// Echo's moveClips: stop previews / close the player on these clips first (Windows os error 32), then move.
pub fn move_ui(a: &AppWindow, lib: &Rc<LibModel>, ids: Vec<usize>, folder: String, sync: Rc<dyn Fn(bool)>) {
    let to = library::clean_folder(&folder);
    let todo: Vec<usize> = ids.into_iter().filter(|&i| i < lib.len() && !lib.clip(i).gone.get() && lib.clip(i).folder != to).collect();
    if todo.is_empty() {
        crate::toast(a, "Already in that folder");
        return;
    }
    // the hover flipbook doesn't hold the file, but the player (mpv) does: let go of it first
    a.global::<Flip>().set_row(-1);
    let wait = if a.get_player_open() && !a.global::<crate::Ed>().get_open() {
        a.global::<Play>().invoke_close();
        250
    } else {
        60
    };
    let (w, lib) = (a.as_weak(), lib.clone());
    slint::Timer::single_shot(Duration::from_millis(wait), move || {
        let Some(a) = w.upgrade() else { return };
        let (n, err) = lib.move_clips(&todo, &to);
        if n > 0 {
            lib.clear_picks();
            let dest = if to.is_empty() { "the main folder".to_string() } else { format!("\"{to}\"") };
            let msg = format!("Moved {n} clip{} to {dest}", if n == 1 { "" } else { "s" });
            crate::toast(&a, match &err {
                Some(e) => format!("{msg} - {e}"),
                None => msg,
            });
        } else if let Some(e) = err {
            crate::toast(&a, e);
        }
        lib.refresh();
        sync(true);
    });
}

pub fn wire(app: &AppWindow, lib: Rc<LibModel>, sync: Rc<dyn Fn(bool)>) {
    {
        let (w, lib) = (app.as_weak(), lib.clone());
        app.global::<Lib>().on_drag_start(move |row| {
            let Some(a) = w.upgrade() else { return };
            // a hover preview keeps the file open - Windows then refuses rename / move / delete (Echo: stopPreviews)
            crate::hover_vid::stop(&a);
            let Some(idx) = lib.index_at(row as usize) else { return };
            let ids = targets(&lib, idx);
            let label = if ids.len() > 1 { format!("{} clips", ids.len()) } else { lib.clip(idx).shown.clone() };
            a.global::<crate::CDrag>().set_label(label.into());
            // Echo: the hover preview keeps the file open - Windows then refuses the move
            a.global::<Flip>().set_row(-1);
            a.invoke_ctrl_off();
        });
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.global::<Lib>().on_drop_to(move |row, folder| {
            let Some(a) = w.upgrade() else { return };
            // a hover preview keeps the file open - Windows then refuses rename / move / delete (Echo: stopPreviews)
            crate::hover_vid::stop(&a);
            let Some(idx) = lib.index_at(row as usize) else { return };
            move_ui(&a, &lib, targets(&lib, idx), folder.to_string(), sync.clone());
        });
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.global::<Lib>().on_move_to(move |row, folder| {
            let Some(a) = w.upgrade() else { return };
            // a hover preview keeps the file open - Windows then refuses rename / move / delete (Echo: stopPreviews)
            crate::hover_vid::stop(&a);
            let Some(idx) = lib.index_at(row as usize) else { return };
            let ids = targets(&lib, idx);
            if folder == "?" {
                let n = ids.len();
                let title = if n == 1 { "Move to a new folder".to_string() } else { format!("Move {n} clips to a new folder") };
                let (lib, sync) = (lib.clone(), sync.clone());
                crate::ask(&a, &title, "A new folder, or a sub-folder like FiveM/Heists.", &[("Cancel", "", ""), ("Move", "ok", "primary")], Some("Name or Folder/Sub"), "ok", move |a, v, text| {
                    if v == "ok" && !library::clean_folder(&text).is_empty() {
                        move_ui(a, &lib, ids, text, sync);
                    }
                });
                return;
            }
            move_ui(&a, &lib, ids, folder.to_string(), sync.clone());
        });
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.global::<Lib>().on_rename(move |row, name| {
            let Some(a) = w.upgrade() else { return };
            // a hover preview keeps the file open - Windows then refuses rename / move / delete (Echo: stopPreviews)
            crate::hover_vid::stop(&a);
            let Some(idx) = lib.index_at(row as usize) else { return };
            let c = lib.clip(idx);
            let v = name.trim();
            if v.is_empty() || v == c.name || v == c.shown {
                return;
            }
            // the player / editor holds the file: close the player first (not the editor - it has its own project)
            if a.get_player_open() && a.global::<crate::Ed>().get_open() {
                crate::toast(&a, "Close the editor first, then rename the clip");
                return;
            }
            let reopen = a.get_player_open();
            if reopen {
                a.global::<Play>().invoke_close();
            }
            let (w, lib, sync, v) = (a.as_weak(), lib.clone(), sync.clone(), v.to_string());
            slint::Timer::single_shot(Duration::from_millis(if reopen { 250 } else { 0 }), move || {
                let Some(a) = w.upgrade() else { return };
                match lib.rename_clip(idx, &v) {
                    Ok(_) => crate::toast(&a, "Renamed"),
                    Err(e) => crate::toast(&a, e),
                }
                lib.refresh();
                sync(false);
            });
        });
    }
}
