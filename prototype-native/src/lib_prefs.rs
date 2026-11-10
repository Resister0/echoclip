//! gaps-library (10 Nov 2026):
//! - row 20: the Library's sort, Grid/List and filter picks are remembered across restarts (Echo's localStorage
//!   "sort" / "libview" / "echo_filters"; here native_ui.json via lib_fx::ui_set)
//! - row 44: sort "Longest" first finds the length of every clip without a .dur file (Echo's thumb call), so all
//!   clips sort correctly
use crate::library::LibModel;
use crate::AppWindow;
use slint::ComponentHandle;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

thread_local! {
    static PROBING: Cell<bool> = const { Cell::new(false) };
}

const SORTS: [&str; 5] = ["new", "old", "big", "long", "name"];

/// once at start: the remembered picks + the filter panel's search box (row 21)
pub fn wire(a: &AppWindow, lib: &Rc<LibModel>, sync: Rc<dyn Fn(bool)>) {
    {
        let (w, lib) = (a.as_weak(), lib.clone());
        a.on_fsearch(move |q| {
            *lib.fq.borrow_mut() = q.to_string();
            if let Some(a) = w.upgrade() {
                a.set_flines(slint::ModelRc::new(slint::VecModel::from(lib.filter_lines())));
            }
        });
    }
    restore(a, lib, sync);
}

/// put back what was picked last time (before the first refresh shows the library)
fn restore(a: &AppWindow, lib: &Rc<LibModel>, sync: Rc<dyn Fn(bool)>) {
    if let Some(s) = crate::lib_fx::ui_get("lib_sort").as_str().filter(|s| SORTS.contains(s)) {
        *lib.sort.borrow_mut() = s.to_string();
        a.set_sort(s.into());
    }
    if let Some(v) = crate::lib_fx::ui_get("lib_view").as_bool() {
        a.set_list_view(v);
    }
    if let Ok(f) = serde_json::from_value::<BTreeMap<String, Vec<String>>>(crate::lib_fx::ui_get("lib_filters")) {
        let f: BTreeMap<String, Vec<String>> = f.into_iter().filter(|(g, k)| matches!(g.as_str(), "game" | "type" | "tag" | "where") && !k.is_empty()).collect();
        if !f.is_empty() {
            *lib.fsel.borrow_mut() = f;
        }
    }
    lib.refresh();
    sync(false);
    if lib.sort.borrow().as_str() == "long" {
        probe_long(a, lib, sync);
    }
}

pub fn save_sort(s: &str) {
    crate::lib_fx::ui_set("lib_sort", serde_json::json!(s));
}

pub fn save_view(list: bool) {
    crate::lib_fx::ui_set("lib_view", serde_json::json!(list));
}

pub fn save_filters(lib: &LibModel) {
    let f: BTreeMap<String, Vec<String>> = lib.fsel.borrow().iter().filter(|(_, k)| !k.is_empty()).map(|(g, k)| (g.clone(), k.clone())).collect();
    crate::lib_fx::ui_set("lib_filters", if f.is_empty() { serde_json::Value::Null } else { serde_json::json!(f) });
}

/// sort "Longest": clips on this PC without a known length get one from ffprobe (in the background), saved as Echo's
/// <thumbs>/<name>.dur so it is known next time too; then the view sorts again.
pub fn probe_long(a: &AppWindow, lib: &Rc<LibModel>, sync: Rc<dyn Fn(bool)>) {
    if PROBING.with(|p| p.get()) {
        return;
    }
    let todo: Vec<(usize, PathBuf, PathBuf)> = {
        let pr = lib.probed.borrow();
        (0..lib.len())
            .map(|i| (i, lib.clip(i)))
            .filter(|(i, c)| c.dur.is_none() && c.cloud.is_none() && !c.gone.get() && !pr.contains_key(i))
            .map(|(i, c)| (i, c.path.clone(), c.thumbs.join(format!("{}.dur", c.name))))
            .collect()
    };
    if todo.is_empty() {
        return;
    }
    PROBING.with(|p| p.set(true));
    let (w, lib) = (a.as_weak(), lib.clone());
    crate::lib_net::bg(
        move || {
            let ff = crate::sheets::ffmpeg();
            let mut out = vec![];
            for (i, p, dur_file) in todo {
                // 0 = couldn't read it: not asked again this run
                let d = crate::tidy::probe_dur(&ff, &p).unwrap_or(0.0);
                if d > 0.0 && !dur_file.exists() {
                    let _ = std::fs::write(&dur_file, format!("{d}"));
                }
                out.push((i, d));
            }
            out
        },
        move |out| {
            PROBING.with(|p| p.set(false));
            lib.probed.borrow_mut().extend(out);
            if w.upgrade().is_some() && lib.sort.borrow().as_str() == "long" {
                lib.refresh();
                sync(false);
            }
        },
    );
}
