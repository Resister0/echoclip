//! Chat D - Echo's "Tidy up" (openTidy): clips saved twice (same game, time ranges overlap > 50 %, the LATEST is
//! pre-marked KEEP - 10 Nov 2026: the newest save holds the last seconds that may matter), very short ones
//! (< 5 s, pre-ticked), the biggest (top 10 over 100 MB, not ticked). Session cleanup (10 Nov 2026) adds two
//! sections, both not ticked: an edit and its original (keep one), and clips that are in the cloud too (only the
//! copy on this PC goes - their cloud copy is never deleted from here). Every section says how much it frees.
//! Favourites are never listed. Delete = Echo's confirm (above this dialog) -> Recycle Bin / Trash.
use crate::library::{self, LibModel};
use crate::{AppWindow, Lib, TRow, Tidy};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::UNIX_EPOCH;

struct Item {
    idx: usize,
    end: f64, // saved at (file time, seconds)
    dur: f64,
    size: u64,
    game: String,
    /// file name without extension (an edit and its original are found by name)
    name: String,
    /// its cloud rel, lower-case, "/" (cloud.json's list); only for clips that are on this PC
    rel: Option<String>,
}

/// "In the cloud too" can be most of the library: the dialog lists the biggest ones (each row decodes a picture)
const CLOUD_MAX: usize = 20;

/// the lists (clip ids). A clip shows in one section only, the first that takes it.
struct Sorted {
    groups: Vec<Vec<usize>>,
    /// per group: the clip to keep + its tag (QA B25, 11 Nov 2026: "KEEP (latest)" / "KEEP (longest)") + the clips
    /// pre-ticked for deletion (only those the kept clip really covers)
    keep: Vec<(usize, &'static str, Vec<usize>)>,
    short: Vec<usize>,
    /// cleanup: an original + its edits (original first)
    pairs: Vec<Vec<usize>>,
    big: Vec<usize>,
    /// cleanup: on this PC with a complete copy in the cloud, biggest first (all of them; the dialog shows the top 20)
    cloud: Vec<usize>,
}

/// Echo's openTidy maths + the cleanup sections
fn sort_out(items: &[Item], cloud_sizes: &HashMap<String, u64>) -> Sorted {
    let (groups, short, big) = sort_dups(items);
    let mut used: std::collections::HashSet<usize> = groups.iter().flatten().chain(short.iter()).copied().collect();
    let named: Vec<(usize, String)> = items.iter().filter(|i| !used.contains(&i.idx)).map(|i| (i.idx, i.name.clone())).collect();
    let pairs = crate::cleanup::edit_pairs(&named);
    used.extend(pairs.iter().flatten().copied());
    let big: Vec<usize> = big.into_iter().filter(|i| !used.contains(i)).collect();
    used.extend(big.iter().copied());
    let mut cloud: Vec<&Item> = items
        .iter()
        .filter(|i| !used.contains(&i.idx) && i.size > 0)
        .filter(|i| i.rel.as_ref().and_then(|r| cloud_sizes.get(r)).is_some_and(|&s| s == i.size))
        .collect();
    cloud.sort_by(|a, b| b.size.cmp(&a.size));
    let by: HashMap<usize, &Item> = items.iter().map(|i| (i.idx, i)).collect();
    let keep = groups.iter().map(|g| keep_of(&g.iter().filter_map(|i| by.get(i).copied()).collect::<Vec<_>>())).collect();
    Sorted { groups, keep, short, pairs, big, cloud: cloud.into_iter().map(|i| i.idx).collect() }
}

/// QA B25 (11 Nov 2026, approved by the coordinator; William's 10 Nov rule was "the latest is kept"): two clips of
/// about the same length (within 25 %) = the hotkey pressed twice -> keep the LATEST (it holds the last seconds that may
/// matter). Different lengths (30 s + 3 min hotkeys) -> the short one lies inside the long one: keep the LONGEST -
/// keeping the latest short clip threw minutes of footage away.
fn similar(a: f64, b: f64) -> bool {
    a.min(b) >= 0.75 * a.max(b)
}

fn keep_of(g: &[&Item]) -> (usize, &'static str, Vec<usize>) {
    let Some(first) = g.first() else { return (usize::MAX, "", vec![]) };
    let latest = g.iter().fold(*first, |a, b| if b.end >= a.end { *b } else { a });
    let longest = g.iter().fold(*first, |a, b| if b.dur > a.dur { *b } else { a });
    let (k, tag) = if similar(latest.dur, longest.dur) { (latest, "KEEP (latest)") } else { (longest, "KEEP (longest)") };
    // groups chain (each clip is compared with the one before): tick only what the KEPT clip covers - a 30-s clip that
    // reaches 8 s past the kept 3-min one stays unticked
    let ticks = g.iter().filter(|m| m.idx != k.idx && covers(k, m)).map(|m| m.idx).collect();
    (k.idx, tag, ticks)
}

/// `a` holds `b`'s footage: about the same length and more than half overlaps, or `b` is shorter and 90 %+ inside `a`
fn covers(a: &Item, b: &Item) -> bool {
    let ov = a.end.min(b.end) - (a.end - a.dur).max(b.end - b.dur);
    let need = if similar(a.dur, b.dur) { 0.5 } else { 0.9 };
    ov > need * a.dur.min(b.dur)
}

/// a group whose clips' lengths differ a lot (its cloud copies are never pre-ticked for deletion)
fn mixed(g: &[usize], durs: &HashMap<usize, f64>) -> bool {
    let d: Vec<f64> = g.iter().map(|i| durs.get(i).copied().unwrap_or(0.0)).collect();
    let (lo, hi) = d.iter().fold((f64::MAX, 0.0f64), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    !d.is_empty() && !similar(lo, hi)
}

/// the groups / short / big lists (clip ids) - Echo's openTidy maths
fn sort_dups(items: &[Item]) -> (Vec<Vec<usize>>, Vec<usize>, Vec<usize>) {
    let by: HashMap<usize, &Item> = items.iter().map(|i| (i.idx, i)).collect();
    let mut games: HashMap<String, Vec<&Item>> = HashMap::new();
    for i in items.iter().filter(|i| i.dur > 0.0) {
        games.entry(i.game.to_lowercase()).or_default().push(i);
    }
    let mut groups: Vec<Vec<usize>> = vec![];
    let mut keys: Vec<&String> = games.keys().collect();
    keys.sort();
    for k in keys {
        let mut g = games[k].clone();
        g.sort_by(|a, b| a.end.partial_cmp(&b.end).unwrap_or(std::cmp::Ordering::Equal));
        let mut cur: Vec<&Item> = vec![];
        for a in g {
            if let Some(prev) = cur.last() {
                // a clip covers [saved - length, saved]
                let (s1, s2) = (prev.end - prev.dur, a.end - a.dur);
                let ov = prev.end.min(a.end) - s1.max(s2);
                // QA B25: same-ish lengths = more than half overlaps (pressed twice); different lengths only when the
                // short clip lies (90 %+) inside the long one
                let need = if similar(prev.dur, a.dur) { 0.5 } else { 0.9 };
                if ov > need * prev.dur.min(a.dur) {
                    cur.push(a);
                    continue;
                }
            }
            if cur.len() > 1 {
                groups.push(cur.iter().map(|i| i.idx).collect());
            }
            cur = vec![a];
        }
        if cur.len() > 1 {
            groups.push(cur.iter().map(|i| i.idx).collect());
        }
    }
    let in_dup: std::collections::HashSet<usize> = groups.iter().flatten().cloned().collect();
    let short: Vec<usize> = items.iter().filter(|i| i.dur > 0.0 && i.dur < 5.0 && !in_dup.contains(&i.idx)).map(|i| i.idx).collect();
    let mut big: Vec<&Item> = items.iter().filter(|i| !in_dup.contains(&i.idx) && !short.contains(&i.idx)).collect();
    big.sort_by(|a, b| b.size.cmp(&a.size));
    let big: Vec<usize> = big.into_iter().take(10).filter(|i| i.size > 100_000_000).map(|i| i.idx).collect();
    let _ = by;
    (groups, short, big)
}

pub fn probe_dur(ff: &Option<PathBuf>, p: &std::path::Path) -> Option<f64> {
    let ff = ff.as_ref()?;
    let mut c = std::process::Command::new(crate::edrender::ffprobe(ff));
    c.args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"]).arg(p);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let o = c.output().ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse().ok().filter(|d: &f64| *d > 0.0)
}

thread_local! {
    static DURS: std::cell::RefCell<HashMap<PathBuf, f64>> = std::cell::RefCell::new(HashMap::new());
}

fn sum(a: &AppWindow, lib: &LibModel) {
    let t = a.global::<Tidy>();
    let rows = t.get_rows();
    let (mut n, mut b) = (0, 0u64);
    for r in rows.iter().filter(|r| r.kind == "clip" && r.on) {
        n += 1;
        if (r.idx as usize) < lib.len() {
            b += lib.clip(r.idx as usize).size;
        }
    }
    t.set_n_on(n);
    t.set_sum(if n > 0 { format!("{n} clip{} · {}", if n == 1 { "" } else { "s" }, library::fmt_size(b)) } else { String::new() }.into());
}

/// open (or refresh) the dialog: durations of clips without Echo's .dur are read with ffprobe in the background
pub fn open(a: &AppWindow, lib: &Rc<LibModel>) {
    let t = a.global::<Tidy>();
    t.set_bin(if cfg!(windows) { "Recycle Bin" } else { "Trash" }.into());
    t.set_open(true);
    t.set_busy(true);
    t.set_rows(ModelRc::new(VecModel::from(Vec::<TRow>::new())));
    t.set_n_on(0);
    t.set_sum("".into());
    // Echo shows the cloud checkbox only when a cloud is set up
    t.set_cloud_box(library::echo_settings().and_then(|s| s.get("cloud").cloned()).is_some_and(|c| !c.is_null() && c != serde_json::json!(false)));
    let favs = lib.favs.borrow().clone();
    let known: HashMap<PathBuf, f64> = DURS.with(|d| d.borrow().clone());
    let list: Vec<(Item, PathBuf, Option<f64>)> = (0..lib.len())
        .map(|i| (i, lib.clip(i)))
        .filter(|(_, c)| !c.gone.get() && !favs.contains(&c.name))
        .map(|(i, c)| {
            let end = c.mtime.duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            let dur = c.dur.or_else(|| known.get(&c.path).copied());
            let rel = c.cloud.is_none().then(|| crate::cloud::rel_of(&c).replace('\\', "/").to_lowercase());
            (Item { idx: i, end, dur: 0.0, size: c.size, game: c.game.clone(), name: c.name.clone(), rel }, c.path.clone(), dur)
        })
        .collect();
    let thumbs = lib.thumbs_dir.clone();
    let w = a.as_weak();
    std::thread::spawn(move || {
        let ff = crate::sheets::ffmpeg();
        let mut found: Vec<(PathBuf, f64)> = vec![];
        let items: Vec<Item> = list
            .into_iter()
            .map(|(mut it, p, dur)| {
                let dur = dur.or_else(|| {
                    let d = probe_dur(&ff, &p);
                    if let Some(d) = d {
                        found.push((p.clone(), d));
                    }
                    d
                });
                it.dur = dur.unwrap_or(0.0);
                it
            })
            .collect();
        let sorted = sort_out(&items, &crate::cleanup::cloud_sizes(&thumbs));
        let durs: HashMap<usize, f64> = items.iter().map(|i| (i.idx, i.dur)).collect();
        let _ = w.upgrade_in_event_loop(move |a| {
            DURS.with(|d| d.borrow_mut().extend(found));
            if let Some(lib) = crate::LIB.with(|l| l.borrow().clone()) {
                fill(&a, &lib, sorted, durs);
            }
        });
    });
}

/// "frees 1.2 GB" / "up to 1.2 GB" for a section's note
fn sz(lib: &LibModel, ids: &[usize]) -> String {
    library::fmt_size(ids.iter().map(|&i| lib.clip(i).size).sum())
}

fn fill(a: &AppWindow, lib: &LibModel, s: Sorted, durs: HashMap<usize, f64>) {
    let Sorted { groups, keep, short, pairs, big, cloud } = s;
    // the cloud box starts ticked each time (QA B25: mixed-length groups untick it below)
    a.global::<Tidy>().set_cloud_too(true);
    let mut gid = 0i32;
    let mut rows: Vec<TRow> = vec![];
    let head = |title: &str, note: String| TRow { kind: "head".into(), title: title.into(), note: note.into(), idx: -1, ..Default::default() };
    let clip_row = |i: usize, on: bool, keep: bool, first: bool, last: bool| {
        let c = lib.clip(i);
        let d = durs.get(&i).copied().unwrap_or(0.0);
        let mut bits = vec![library::fmt_size(c.size)];
        if d > 0.0 {
            bits.push(library::fmt_dur(d));
        }
        bits.push(library::ago(c.mtime));
        if !c.game.is_empty() {
            bits.push(c.game.clone());
        }
        if lib.in_cloud(i) {
            bits.push("also in the cloud".into());
        }
        let thumb = library::decode(&c.thumb()).map(slint::Image::from_rgb8).unwrap_or_default();
        TRow {
            kind: "clip".into(),
            title: c.shown.clone().into(),
            note: bits.join(" · ").into(),
            idx: i as i32,
            on,
            keep,
            group: -1,
            first,
            last,
            thumb,
            ..Default::default()
        }
    };
    if !groups.is_empty() {
        let n = groups.len();
        // 10 Nov 2026: the LATEST save is kept (it holds the last seconds that may matter), not the longest
        // (Echo's was g.reduce((a, b) => dur(b) > dur(a) ? b : a)). QA B25 (11 Nov 2026): only for about the same
        // length - otherwise the longest (sort_out / keep_of). Mixed lengths: their cloud copies aren't pre-ticked.
        let frees: u64 = keep.iter().map(|k| k.2.iter().map(|&i| lib.clip(i).size).sum::<u64>()).sum();
        rows.push(head("SAVED TWICE", format!("{n} group{} - hotkey pressed twice, or a short clip inside a longer one. The latest is kept, or the longest when the lengths differ · frees {}", if n == 1 { "" } else { "s" }, library::fmt_size(frees))));
        if groups.iter().any(|g| mixed(g, &durs)) {
            a.global::<Tidy>().set_cloud_too(false);
        }
        for (gi, g) in groups.iter().enumerate() {
            if gi > 0 {
                rows.push(TRow { kind: "gap".into(), idx: -1, ..Default::default() });
            }
            let (keep, tag, ticks) = &keep[gi];
            let (keep, tag) = (*keep, *tag);
            for (k, &i) in g.iter().enumerate() {
                let mut r = clip_row(i, ticks.contains(&i), i == keep, k == 0, k + 1 == g.len());
                if i == keep {
                    r.tag = tag.into();
                }
                r.group = gi as i32;
                rows.push(r);
            }
        }
        gid = groups.len() as i32;
    }
    if !short.is_empty() {
        rows.push(head("VERY SHORT", format!("Under 5 seconds - often saved by accident · frees {}", sz(lib, &short))));
        for (k, &i) in short.iter().enumerate() {
            rows.push(clip_row(i, true, false, k == 0, k + 1 == short.len()));
        }
    }
    // cleanup (10 Nov 2026): an edit and its original - keep one. Never ticked (and never both).
    if !pairs.is_empty() {
        let orig: Vec<usize> = pairs.iter().map(|g| g[0]).collect();
        rows.push(head("EDIT AND ORIGINAL", format!("Not selected - keep the edit or the original. Keeping only the edits frees {}", sz(lib, &orig))));
        for (pi, g) in pairs.iter().enumerate() {
            if pi > 0 {
                rows.push(TRow { kind: "gap".into(), idx: -1, ..Default::default() });
            }
            for (k, &i) in g.iter().enumerate() {
                let mut r = clip_row(i, false, false, k == 0, k + 1 == g.len());
                r.tag = if k == 0 { "ORIGINAL" } else { "EDIT" }.into();
                r.group = gid;
                rows.push(r);
            }
            gid += 1;
        }
    }
    if !big.is_empty() {
        rows.push(head("BIGGEST CLIPS", format!("Not selected - pick the ones you don't need (or make them smaller from the right-click menu) · up to {}", sz(lib, &big))));
        for (k, &i) in big.iter().enumerate() {
            rows.push(clip_row(i, false, false, k == 0, k + 1 == big.len()));
        }
    }
    // cleanup: clips with a full copy in the cloud - only the copy on this PC goes (the cloud copy stays, whatever
    // "Also delete their cloud copies" says), for people who use the cloud as the place to keep old clips
    if !cloud.is_empty() {
        let n = cloud.len();
        let shown: Vec<usize> = cloud.iter().copied().take(CLOUD_MAX).collect();
        let more = if n > CLOUD_MAX { format!(" - the {CLOUD_MAX} biggest of {n} shown") } else { String::new() };
        rows.push(head("IN THE CLOUD TOO", format!("Not selected - they leave this PC and stay in the cloud{more} · up to {}", sz(lib, &shown))));
        for (k, &i) in shown.iter().enumerate() {
            let mut r = clip_row(i, false, false, k == 0, k + 1 == shown.len());
            r.act = "pc".into();
            rows.push(r);
        }
    }
    let t = a.global::<Tidy>();
    t.set_rows(ModelRc::new(VecModel::from(rows)));
    t.set_busy(false);
    sum(a, lib);
}

pub fn wire(app: &AppWindow, lib: Rc<LibModel>) {
    {
        let (w, lib) = (app.as_weak(), lib.clone());
        app.global::<Tidy>().on_toggle(move |row| {
            let Some(a) = w.upgrade() else { return };
            let rows = a.global::<Tidy>().get_rows();
            if let Some(mut r) = rows.row_data(row as usize) {
                if r.kind == "clip" {
                    r.on = !r.on;
                    rows.set_row_data(row as usize, r);
                }
            }
            sum(&a, &lib);
        });
    }
    {
        let (w, lib) = (app.as_weak(), lib.clone());
        app.global::<Tidy>().on_play(move |idx| {
            let Some(a) = w.upgrade() else { return };
            a.global::<Tidy>().set_open(false);
            let idx = idx as usize;
            if lib.view_row_of(idx).is_none() {
                a.invoke_pick_filter("all".into());
            }
            if let Some(row) = lib.view_row_of(idx) {
                lib.clear_picks();
                a.global::<Lib>().invoke_open(row as i32, false);
            }
        });
    }
    {
        let (w, lib) = (app.as_weak(), lib.clone());
        app.global::<Tidy>().on_delete(move || {
            let Some(a) = w.upgrade() else { return };
            let t = a.global::<Tidy>();
            let picked: Vec<TRow> = t.get_rows().iter().filter(|r| r.kind == "clip" && r.on && (r.idx as usize) < lib.len()).collect();
            let ids: Vec<usize> = picked.iter().map(|r| r.idx as usize).collect();
            if ids.is_empty() {
                return;
            }
            // cleanup: "In the cloud too" rows keep their cloud copy, whatever the checkbox says
            let pc_only: std::collections::HashSet<usize> = picked.iter().filter(|r| r.act == "pc").map(|r| r.idx as usize).collect();
            let n = ids.len();
            let bytes: u64 = ids.iter().map(|&i| lib.clip(i).size).sum();
            let cloud_n = ids.iter().filter(|&&i| lib.in_cloud(i) && !pc_only.contains(&i)).count();
            let also = t.get_cloud_box() && t.get_cloud_too() && cloud_n > 0;
            let bin = if cfg!(windows) { "Recycle Bin" } else { "Trash" };
            // gaps-cloud (row 22): Echo's text - clips only in the cloud (Biggest clips can list them) go for good
            let k = pc_only.len();
            let msg = format!(
                "{}. Clips on this PC go to the {bin}{}. Clips only in the cloud are deleted for good.{}",
                library::fmt_size(bytes),
                if also { "; their cloud copies are deleted for good" } else { "" },
                if k > 0 { format!(" {k} clip{} from \"In the cloud too\" stay{} in the cloud.", if k == 1 { "" } else { "s" }, if k == 1 { "s" } else { "" }) } else { String::new() }
            );
            let lib = lib.clone();
            crate::confirm_box(&a, &format!("Delete {n} clip{}?", if n == 1 { "" } else { "s" }), &msg, "Delete", move |a| {
                let (only, ids): (Vec<usize>, Vec<usize>) = ids.iter().copied().partition(|&i| lib.clip(i).cloud.is_some());
                crate::cloud::delete_cloud_only_now(a, only.clone());
                // Chat B: "Also delete their cloud copies" is real (Echo's cloud_delete)
                let rels: Vec<String> = if also { ids.iter().filter(|&&i| lib.in_cloud(i) && !pc_only.contains(&i)).map(|&i| crate::cloud::rel_of(&lib.clip(i))).collect() } else { vec![] };
                if !rels.is_empty() {
                    if let Some(r) = crate::rec_core::rec() {
                        std::thread::spawn(move || {
                            for rel in rels {
                                let _ = r.cloud_delete(&rel);
                            }
                        });
                    }
                }
                // r2-threads (L5): the Recycle Bin / Trash on a worker, with Echo's "Deleting clips…" box
                let total = ids.len();
                let bx = (total > 1).then(|| {
                    let id = crate::prog_box("Deleting clips…");
                    crate::prog_set(id, None, Some(&format!("0 of {total}")), Some(0.0));
                    id
                });
                let (w, l2) = (a.as_weak(), lib.clone());
                let only_n = only.len();
                lib.delete_clips_bg(
                    &ids,
                    move |i, total| {
                        if let Some(id) = bx {
                            crate::prog_set(id, None, Some(&format!("{i} of {total}")), Some(i as f32 / total as f32 * 100.0));
                        }
                    },
                    move |k, err| {
                        if let Some(id) = bx {
                            crate::prog_done(id, 300);
                        }
                        let Some(a) = w.upgrade() else { return };
                        let a = &a;
                        if let Some(e) = err {
                            crate::toast(a, e);
                        }
                        let k = k + only_n;
                        crate::toast(a, format!("{k} clip{} deleted", if k == 1 { "" } else { "s" }));
                        l2.refresh();
                        crate::UI_SYNC.with(|f| {
                            if let Some(f) = f.borrow().as_ref() {
                                f(true)
                            }
                        });
                        open(a, &l2);
                    },
                );
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(idx: usize, end: f64, dur: f64) -> Item {
        Item { idx, end, dur, size: 1_000_000, game: "GameX".into(), name: format!("c{idx}"), rel: None }
    }

    /// QA B25: the 3-min hotkey, then the 30-s one 2 s later (93 % of it inside) - keep the 3-min clip, tick the 30-s one
    #[test]
    fn long_then_short_keeps_the_long_one() {
        let items = vec![it(0, 1000.0, 180.0), it(1, 1002.0, 30.0)];
        let (groups, _, _) = sort_dups(&items);
        assert_eq!(groups, vec![vec![0, 1]]);
        let g: Vec<&Item> = items.iter().collect();
        assert_eq!(keep_of(&g), (0, "KEEP (longest)", vec![1]));
        let durs: HashMap<usize, f64> = items.iter().map(|i| (i.idx, i.dur)).collect();
        assert!(mixed(&groups[0], &durs));
    }

    /// the hotkey pressed twice (same length): the latest is kept, as before
    #[test]
    fn pressed_twice_keeps_the_latest() {
        let items = vec![it(0, 1000.0, 30.0), it(1, 1012.0, 30.0)];
        let (groups, _, _) = sort_dups(&items);
        assert_eq!(groups, vec![vec![0, 1]]);
        let g: Vec<&Item> = items.iter().collect();
        assert_eq!(keep_of(&g), (1, "KEEP (latest)", vec![0]));
        let durs: HashMap<usize, f64> = items.iter().map(|i| (i.idx, i.dur)).collect();
        assert!(!mixed(&groups[0], &durs));
    }

    /// a short clip only partly inside a long one (< 90 %) isn't a duplicate: both stay
    #[test]
    fn short_sticking_out_is_not_grouped() {
        // long covers [820, 1000]; short covers [980, 1010]: 20 of its 30 s inside = 67 %
        let items = vec![it(0, 1000.0, 180.0), it(1, 1010.0, 30.0)];
        let (groups, _, _) = sort_dups(&items);
        assert!(groups.is_empty());
    }

    /// a 10-min clip and an 11-s clip that only touch at the end (5 s of it inside) aren't a pair
    #[test]
    fn ten_minutes_and_eleven_seconds_touching_are_not_grouped() {
        let items = vec![it(0, 1000.0, 600.0), it(1, 1006.0, 11.0)];
        let (groups, _, _) = sort_dups(&items);
        assert!(groups.is_empty());
        // fully inside (saved earlier): grouped, the 10-min clip kept
        let items = vec![it(0, 1000.0, 600.0), it(1, 900.0, 11.0)];
        let (groups, _, _) = sort_dups(&items);
        assert_eq!(groups, vec![vec![1, 0]]);
        let g: Vec<&Item> = groups[0].iter().map(|&i| &items[i]).collect();
        assert_eq!(keep_of(&g), (0, "KEEP (latest)", vec![1])); // the 10-min clip is also the latest
    }

    /// a chain: 3 min, a 30 s inside it, another 30 s 8 s later (joined through the first 30 s) - only the one the
    /// kept 3-min clip covers is ticked; the last one holds footage the 3-min clip doesn't have
    #[test]
    fn chain_ticks_only_what_the_kept_clip_covers() {
        let items = vec![it(0, 1000.0, 180.0), it(1, 1002.0, 30.0), it(2, 1010.0, 30.0)];
        let (groups, _, _) = sort_dups(&items);
        assert_eq!(groups, vec![vec![0, 1, 2]]);
        let g: Vec<&Item> = items.iter().collect();
        assert_eq!(keep_of(&g), (0, "KEEP (longest)", vec![1]));
    }
}
