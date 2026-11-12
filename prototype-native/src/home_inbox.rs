//! Home "Inbox" (11 Nov 2026, the user's pick from the Home mockups): Home shows ONE clip big - the newest clip not
//! looked at yet ("new") - with a "3 of 5" pager through the new ones, buttons + the clip's details beside it, and
//! "Your games" shelves under it.
//!
//! The clip on show is Home card 0 (`LibModel::set_home(vec![it])`), so every existing Home action (Copy for Discord
//! + its size menu, Share link, Trim / edit straight into the editor, Captions, play, the sharp still, the hover
//! video sized by `HomeState.hero-w`) works on it unchanged.
//!
//! "Seen" (stored in native_ui.json, never in the clips): a time + the names of newer clips already looked at.
//! Everything saved before that time counts as seen; when no new clip is left the time moves up and the names are
//! dropped, so the list stays small. The first start with this Home marks everything older than a day as seen.
//! A clip becomes seen by Keep, Delete, Next / → or by sending / editing / playing it.

use crate::library::{self, Clip, LibModel};
use crate::{AppWindow, HomeState};
use slint::{ComponentHandle, Image, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

const KEY_T: &str = "home_seen_t";
const KEY_N: &str = "home_seen";
const KEY_ROOT: &str = "home_seen_root"; // QA B31: the clips folder the seen state belongs to
const FIRST_RUN_BACK: u64 = 24 * 3600;

struct Seen {
    t: u64,
    names: HashSet<String>,
}

/// what ffprobe said about a clip's video (None = no video stream / not readable)
#[derive(Clone)]
struct Probe {
    w: u32,
    h: u32,
    fps: f64,
    codec: String,
    /// seconds (the library may not know a new clip's length yet)
    dur: f64,
}

thread_local! {
    static SEEN: RefCell<Option<Seen>> = const { RefCell::new(None) };
    /// the clips folder Home works on (set by fill): another folder = another seen state
    static ROOT: RefCell<String> = const { RefCell::new(String::new()) };
    /// clips looked at during this run: they stay in the "3 of 5" list (as done) while new ones are left
    static RUN: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// the clip on show (by name: ids change when the library reloads) + its place in the list
    static CUR: RefCell<String> = const { RefCell::new(String::new()) };
    static CUR_POS: Cell<usize> = const { Cell::new(0) };
    /// the list the pager goes through (clip ids, newest first) + whether those are new clips
    static LIST: RefCell<(Vec<usize>, bool)> = const { RefCell::new((Vec::new(), false)) };
    /// ffprobe results by file (Some(None) = probed, nothing found; missing = not probed yet)
    static DET: RefCell<HashMap<PathBuf, Option<Probe>>> = RefCell::new(HashMap::new());
    static PROBING: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
    /// shelf pictures by file + its modification time
    static PICS: RefCell<HashMap<PathBuf, (SystemTime, Option<Image>)>> = RefCell::new(HashMap::new());
    /// the last fill's list (clip keys) + whether it was the new clips; None = no fill yet (QA E7, E14)
    static PREV: RefCell<Option<(HashSet<String>, bool)>> = const { RefCell::new(None) };
    /// when the user last paged (← → Keep) - new clips arriving don't move the pager while they page (QA E14)
    static LAST_ACT: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

/// clips that arrive while the user isn't paging: Home goes to the newest one (QA E14)
const PAGING_QUIET: std::time::Duration = std::time::Duration::from_secs(60);

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn with_seen<R>(f: impl FnOnce(&mut Seen) -> R) -> R {
    SEEN.with(|s| {
        let mut s = s.borrow_mut();
        if s.is_none() {
            // QA B31: saved for another clips folder (the folder was changed) = start over for this one. A state saved
            // before the folder was stored (no home_seen_root yet) is taken for the current folder.
            let root = ROOT.with(|r| r.borrow().clone());
            let saved_root = crate::lib_fx::ui_get(KEY_ROOT).as_str().map(String::from);
            let same = saved_root.as_deref().is_none_or(|r| r == root);
            let t = crate::lib_fx::ui_get(KEY_T).as_u64().filter(|_| same);
            let names: HashSet<String> = if same {
                crate::lib_fx::ui_get(KEY_N).as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
            } else {
                HashSet::new()
            };
            let fresh = t.is_none() || saved_root.is_none();
            let t = t.unwrap_or_else(|| secs(SystemTime::now()).saturating_sub(FIRST_RUN_BACK));
            *s = Some(Seen { t, names });
            if fresh {
                let names: Vec<&String> = s.as_ref().map(|x| x.names.iter().collect()).unwrap_or_default();
                crate::lib_fx::ui_set(KEY_T, serde_json::json!(t));
                crate::lib_fx::ui_set(KEY_ROOT, serde_json::json!(root));
                crate::lib_fx::ui_set(KEY_N, if names.is_empty() { serde_json::Value::Null } else { serde_json::json!(names) });
            }
        }
        f(s.as_mut().unwrap())
    })
}

fn save_seen() {
    with_seen(|s| {
        let mut v: Vec<&String> = s.names.iter().collect();
        v.sort();
        crate::lib_fx::ui_set(KEY_T, serde_json::json!(s.t));
        crate::lib_fx::ui_set(KEY_ROOT, serde_json::json!(ROOT.with(|r| r.borrow().clone())));
        crate::lib_fx::ui_set(KEY_N, if v.is_empty() { serde_json::Value::Null } else { serde_json::json!(v) });
    });
}

/// clips that can be "new": real clips saved by Echo on this PC (not edited copies the user made, not cloud-only
/// clips from another PC, not sound files)
fn can_be_new(c: &Clip) -> bool {
    !c.edited && !c.sound && c.cloud.is_none() && !c.gone.get()
}

/// a clip's key in the seen list and this run's list: "p:" + its place in the clips folder ("p:Valorant/Clip 1.mp4").
/// QA E1 (11 Nov 2026): by name, two clips with one file name in two folders were one - seeing one hid the other.
fn key(c: &Clip) -> String {
    let file = c.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| c.name.clone());
    if c.folder.is_empty() { format!("p:{file}") } else { format!("p:{}/{file}", c.folder) }
}

fn is_seen(c: &Clip) -> bool {
    // a bare name (no "p:") was saved by an older Echo: it still counts for clips of that name, until the list is
    // cleared (no new clip left)
    with_seen(|s| secs(c.mtime) <= s.t || s.names.contains(&key(c)) || s.names.contains(&c.name))
}

fn is_new(c: &Clip) -> bool {
    can_be_new(c) && !is_seen(c)
}

/// mark one clip seen (and remember it for this run's list). Saves at once (a tiny file).
fn mark_seen(lib: &LibModel, idx: usize) {
    let c = lib.clip(idx);
    if !is_new(&c) {
        return;
    }
    with_seen(|s| {
        s.names.insert(key(&c));
    });
    RUN.with(|r| r.borrow_mut().insert(key(&c)));
    // no new clip left: move the time up to the newest clip and forget the names (keeps the file small)
    let all = lib.all();
    if !all.iter().any(|c| is_new(c)) {
        let newest = all.iter().map(|c| secs(c.mtime)).max().unwrap_or(0);
        with_seen(|s| {
            s.t = s.t.max(newest);
            s.names.clear();
        });
    }
    save_seen();
}

/// the pager's list (clip ids, newest first) and whether it is the new clips (else: every clip, newest first)
fn build_list(lib: &LibModel) -> (Vec<usize>, bool) {
    let n = lib.len();
    let mut ids: Vec<usize> = (0..n).filter(|&i| !lib.clip(i).gone.get()).collect();
    ids.sort_by(|&a, &b| lib.clip(b).mtime.cmp(&lib.clip(a).mtime).then(b.cmp(&a)));
    let any_new = ids.iter().any(|&i| is_new(&lib.clip(i)));
    if !any_new {
        RUN.with(|r| r.borrow_mut().clear());
        return (ids, false);
    }
    let run = RUN.with(|r| r.borrow().clone());
    let list: Vec<usize> = ids
        .into_iter()
        .filter(|&i| {
            let c = lib.clip(i);
            is_new(&c) || run.contains(&key(&c))
        })
        .collect();
    (list, true)
}

/// the clip on show: the remembered one if it is still in the list, else (new clips) the first new one from where it
/// was, else the one now at its old place
fn pick_current(lib: &LibModel, list: &[usize], new_mode: bool) -> Option<usize> {
    if list.is_empty() {
        return None;
    }
    let cur = CUR.with(|c| c.borrow().clone());
    // by full path, not name: two clips can share a file name (a copy in another folder) - matching by name sent the
    // pager back to the first one, so Next stopped there ("25 of 34", 11 Nov 2026)
    if let Some(p) = list.iter().position(|&i| lib.clip(i).path.to_string_lossy() == cur) {
        return Some(p);
    }
    let from = CUR_POS.with(|p| p.get()).min(list.len() - 1);
    if new_mode {
        let unseen = |p: &usize| is_new(&lib.clip(list[*p]));
        if let Some(p) = (from..list.len()).find(unseen).or_else(|| (0..from).find(unseen)) {
            return Some(p);
        }
    }
    Some(if cur.is_empty() { 0 } else { from })
}

fn set_current(lib: &LibModel, list: &[usize], pos: usize) {
    if let Some(&i) = list.get(pos) {
        CUR.with(|c| *c.borrow_mut() = lib.clip(i).path.to_string_lossy().to_string());
        CUR_POS.with(|p| p.set(pos));
    }
}

/// the clip on show (clip id)
pub fn current(lib: &LibModel) -> Option<usize> {
    lib.index_at(library::HOME_BASE)
}

/// Home's data: the clip on show (as card 0), label, pager, details, shelves. Cheap; ffprobe runs on a thread once
/// per file. `refill` is called when a probe finished (it fills Home again).
pub fn fill(a: &AppWindow, lib: &Rc<LibModel>, refill: Rc<dyn Fn()>) {
    // QA B31: the clips folder changed (Settings) -> its own seen state; this run's list starts over
    let root = lib.root().to_string_lossy().to_string();
    if ROOT.with(|r| *r.borrow() != root) {
        ROOT.with(|r| *r.borrow_mut() = root);
        SEEN.with(|s| *s.borrow_mut() = None);
        RUN.with(|r| r.borrow_mut().clear());
        CUR.with(|c| c.borrow_mut().clear());
        CUR_POS.with(|p| p.set(0));
        PREV.with(|p| *p.borrow_mut() = None);
    }
    let (list, new_mode) = build_list(lib);
    let keys: HashSet<String> = list.iter().map(|&i| key(&lib.clip(i))).collect();
    if let Some((prev_keys, prev_new)) = PREV.with(|p| p.borrow_mut().take()) {
        // QA E7: the new clips ran out without Keep / → (the last one deleted or moved): back to the latest clip, as
        // after Keep on the last one - was: the old place in the list of all clips ("YOUR CLIPS · 2 of 12")
        // (only when the clip on show left the list - Copy / Share on the last new clip keep it on show)
        let cur = CUR.with(|c| c.borrow().clone());
        let cur_gone = !list.iter().any(|&i| lib.clip(i).path.to_string_lossy() == cur);
        let ran_out = prev_new && !new_mode && cur_gone;
        // QA E14: new clips arrived (saved while Echo sat in the back): start at the newest one - was: the pager stayed
        // on the first one that came in ("3 of 3" = the oldest). Not while the user is paging.
        let quiet = LAST_ACT.with(|t| t.get().is_none_or(|t| t.elapsed() > PAGING_QUIET));
        let arrived = new_mode && quiet && keys.iter().any(|k| !prev_keys.contains(k));
        if ran_out || arrived {
            CUR.with(|c| c.borrow_mut().clear());
            CUR_POS.with(|p| p.set(0));
        }
    }
    PREV.with(|p| *p.borrow_mut() = Some((keys, new_mode)));
    let pos = pick_current(lib, &list, new_mode);
    let h = a.global::<HomeState>();
    match pos {
        Some(p) => {
            set_current(lib, &list, p);
            lib.set_home(vec![list[p]]);
        }
        None => lib.set_home(vec![]),
    }
    LIST.with(|l| *l.borrow_mut() = (list.clone(), new_mode));
    let pos = pos.unwrap_or(0);
    h.set_new_mode(new_mode);
    h.set_test_hover(std::env::var_os("ECHO_HOME_HOVER").is_some());
    h.set_label(
        if new_mode {
            "NEW SINCE YOU LAST LOOKED"
        } else if pos == 0 {
            "YOUR LATEST CLIP"
        } else {
            "YOUR CLIPS"
        }
        .into(),
    );
    h.set_pos(pos as i32);
    h.set_count(list.len() as i32);
    h.set_can_prev(pos > 0);
    h.set_can_next(pos + 1 < list.len() || (new_mode && list.iter().any(|&i| is_new(&lib.clip(i)))));
    // dots (only for a short list of new clips): 0 = done, 1 = on show, 2 = still new
    let dots: Vec<i32> = if new_mode && list.len() <= 12 {
        list.iter().enumerate().map(|(k, &i)| if k == pos { 1 } else if is_new(&lib.clip(i)) { 2 } else { 0 }).collect()
    } else {
        vec![]
    };
    h.set_dots(ModelRc::new(VecModel::from(dots)));
    if let Some(i) = current(lib) {
        details(a, lib, i, refill);
    }
    shelves(a, lib);
}

fn details(a: &AppWindow, lib: &LibModel, idx: usize, refill: Rc<dyn Fn()>) {
    let c = lib.clip(idx);
    let h = a.global::<HomeState>();
    let probe = DET.with(|d| d.borrow().get(&c.path).cloned());
    let local = c.cloud.is_none() || c.path.exists();
    if probe.is_none() && local {
        start_probe(c.path.clone(), refill);
    }
    let waiting = probe.is_none() && local;
    let p = probe.flatten();
    let dash = if waiting { "…" } else { "-" };
    let dur = c.dur.or(p.as_ref().map(|p| p.dur).filter(|d| *d > 0.0)).map(library::fmt_dur).unwrap_or_else(|| dash.into());
    let size = library::fmt_size(c.size);
    let ext = c.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
    let res = p.as_ref().filter(|p| p.w > 0).map(|p| format!("{} × {}", p.w, p.h)).unwrap_or_else(|| dash.into());
    let fps = p.as_ref().filter(|p| p.fps > 0.5).map(|p| format!("{} fps", fps_text(p.fps))).unwrap_or_else(|| dash.into());
    let fmt = match p.as_ref().map(|p| codec_text(&p.codec)) {
        Some(cd) if !cd.is_empty() && !ext.is_empty() => format!("{ext} ({cd})"),
        _ if !ext.is_empty() => ext.clone(),
        _ => dash.into(),
    };
    let folder = if c.folder.is_empty() { "Clips folder".to_string() } else { c.folder.rsplit('/').next().unwrap_or(&c.folder).to_string() };
    let pic = p.as_ref().filter(|p| p.h > 0).map(|p| format!("{} · {} fps", res_short(p.w, p.h), fps_text(p.fps))).unwrap_or_else(|| dash.into());
    let rows_l = ["Duration", "Size", "Resolution", "Frame rate", "Format", "Folder"];
    let rows_v = [dur.clone(), size.clone(), res, fps, fmt.clone(), folder.clone()];
    let short_l = ["Length", "Picture", "Format", "Folder"];
    let short_v = [format!("{dur} · {size}"), pic, fmt, folder];
    let ss = |v: &[&str]| ModelRc::new(VecModel::from(v.iter().map(|s| SharedString::from(*s)).collect::<Vec<_>>()));
    let so = |v: &[String]| ModelRc::new(VecModel::from(v.iter().map(|s| SharedString::from(s.as_str())).collect::<Vec<_>>()));
    h.set_det_l(ss(&rows_l));
    h.set_det_v(so(&rows_v));
    h.set_det_sl(ss(&short_l));
    h.set_det_sv(so(&short_v));
    // "FiveM · 32 min ago" under the title
    let game = c.game.split('/').next().unwrap_or("").to_string();
    h.set_hero_meta([game, library::ago(c.mtime)].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ").into());
}

/// QA B30: ffprobe's AVERAGE frame rate of a 60 fps recording is often 59.6-59.9 (uneven frames): show the usual rate
/// it was recorded at when it is within 2 % of one (59.94 / 29.97 shown as 60 / 30, like games do), else the number
fn fps_text(f: f64) -> String {
    const STD: [f64; 12] = [23.976, 24.0, 25.0, 29.97, 30.0, 48.0, 50.0, 59.94, 60.0, 120.0, 144.0, 240.0];
    let near = STD.iter().copied().filter(|s| (f - s).abs() / s < 0.02).min_by(|a, b| (f - a).abs().total_cmp(&(f - b).abs()));
    match near {
        Some(s) if (s - 23.976).abs() < 0.001 => "23.976".into(),
        Some(s) => format!("{:.0}", s.round()),
        None if (f - f.round()).abs() < 0.05 => format!("{:.0}", f.round()),
        None => format!("{f:.2}"),
    }
}

fn res_short(w: u32, h: u32) -> String {
    let s = w.min(h);
    match s {
        2160 => "4K".into(),
        0 => "-".into(),
        _ => format!("{s}p"),
    }
}

fn codec_text(c: &str) -> String {
    match c {
        "h264" => "H.264".into(),
        "hevc" | "h265" => "HEVC".into(),
        "av1" => "AV1".into(),
        "vp9" => "VP9".into(),
        "" => String::new(),
        x => x.to_uppercase(),
    }
}

fn start_probe(path: PathBuf, refill: Rc<dyn Fn()>) {
    if !PROBING.with(|p| p.borrow_mut().insert(path.clone())) {
        return;
    }
    let Some(ff) = crate::sheets::ffmpeg() else {
        DET.with(|d| d.borrow_mut().insert(path.clone(), None));
        return;
    };
    REFILL.with(|r| *r.borrow_mut() = Some(refill));
    std::thread::spawn(move || {
        let o = crate::edrender::hidden_cmd(&crate::edrender::ffprobe(&ff))
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height,avg_frame_rate:format=duration", "-of", "default=nw=1"])
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .output();
        let mut p = Probe { w: 0, h: 0, fps: 0.0, codec: String::new(), dur: 0.0 };
        if let Ok(o) = o {
            for l in String::from_utf8_lossy(&o.stdout).lines() {
                let Some((k, v)) = l.split_once('=') else { continue };
                match k.trim() {
                    "codec_name" => p.codec = v.trim().to_string(),
                    "width" => p.w = v.trim().parse().unwrap_or(0),
                    "height" => p.h = v.trim().parse().unwrap_or(0),
                    "duration" => p.dur = v.trim().parse().unwrap_or(0.0),
                    "avg_frame_rate" => {
                        let (n, d) = v.trim().split_once('/').unwrap_or((v.trim(), "1"));
                        let (n, d) = (n.parse::<f64>().unwrap_or(0.0), d.parse::<f64>().unwrap_or(0.0));
                        p.fps = if d > 0.0 { n / d } else { 0.0 };
                    }
                    _ => {}
                }
            }
        }
        let found = (p.w > 0 || !p.codec.is_empty()).then_some(p);
        let _ = slint::invoke_from_event_loop(move || {
            DET.with(|d| d.borrow_mut().insert(path.clone(), found));
            PROBING.with(|p| p.borrow_mut().remove(&path));
            // fill Home again (the details show up); only if Home still shows this clip is not checked: a refill is cheap
            if let Some(f) = REFILL.with(|r| r.borrow().clone()) {
                f();
            }
        });
    });
}

thread_local! {
    /// fills Home again after a probe (set by the last fill that started one)
    static REFILL: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// "Your games": every top folder with clips (the library's FOLDERS list), the most recently played first, each with
/// its icon, "N clips · size" and its newest 1-3 pictures. Open = the library on that folder.
fn shelves(a: &AppWindow, lib: &LibModel) {
    let h = a.global::<HomeState>();
    let all = lib.all();
    struct Sh {
        id: String,
        name: String,
        sub: String,
        icon: Option<Image>,
        newest: SystemTime,
        pics: Vec<Image>,
        game: bool,
    }
    // QA B30: which top folders are games - Echo detected the game (games.json), it has a game icon, a game's clips go
    // there (folder_alias), or Echo named its clips after it ("FiveM 2026-11-08 ..."). Others ("Mixed", "escape") are
    // plain folders: a folder icon and "folder · N clips", like the Evidence shelf in the approved mockup.
    let games: HashSet<String> = library::json_cached(&crate::settings::path().with_file_name("games.json"))
        .and_then(|v| v.as_object().map(|m| m.keys().map(|k| k.to_lowercase()).collect()))
        .unwrap_or_default();
    let alias: HashSet<String> = library::echo_settings()
        .and_then(|v| v.get("folder_alias").and_then(|m| m.as_object().map(|m| m.values().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect())))
        .unwrap_or_default();
    let mut out: Vec<Sh> = vec![];
    for n in lib.nav_items().into_iter().filter(|n| n.id.starts_with("game:") && n.depth == 0) {
        let f = n.folder.to_string();
        let mut clips: Vec<&Rc<Clip>> = all.iter().filter(|c| c.game == f || c.folder == f || c.folder.starts_with(&format!("{f}/"))).collect();
        if clips.is_empty() {
            continue;
        }
        clips.sort_by(|x, y| y.mtime.cmp(&x.mtime));
        let bytes: u64 = clips.iter().map(|c| c.size).sum();
        let k = clips.len();
        let pics: Vec<Image> = clips.iter().filter_map(|c| pic(&c.thumb())).take(3).collect();
        let fl = f.to_lowercase();
        // "<folder> 2026-..." = a clip Echo named after this game
        let named = |c: &&Rc<Clip>| {
            let nl = c.name.to_lowercase();
            nl.strip_prefix(&fl).is_some_and(|rest| rest.starts_with(" 20") && rest.chars().nth(5).is_some_and(|d| d == '-'))
        };
        let game = n.has_img || games.contains(&fl) || alias.contains(&fl) || clips.iter().any(named);
        let many = format!("{k} clip{}", if k == 1 { "" } else { "s" });
        out.push(Sh {
            id: n.id.to_string(),
            name: n.label.to_string(),
            sub: if game { format!("{many} · {}", library::fmt_size(bytes)) } else { format!("folder · {many}") },
            icon: n.has_img.then(|| n.img.clone()),
            newest: clips[0].mtime,
            pics,
            game,
        });
    }
    // games first (most recently played first), then plain folders (newest first)
    out.sort_by(|x, y| y.game.cmp(&x.game).then(y.newest.cmp(&x.newest)));
    let s = |v: Vec<String>| ModelRc::new(VecModel::from(v.into_iter().map(SharedString::from).collect::<Vec<_>>()));
    h.set_sh_id(s(out.iter().map(|x| x.id.clone()).collect()));
    h.set_sh_name(s(out.iter().map(|x| x.name.clone()).collect()));
    h.set_sh_sub(s(out.iter().map(|x| x.sub.clone()).collect()));
    h.set_sh_has_icon(ModelRc::new(VecModel::from(out.iter().map(|x| x.icon.is_some()).collect::<Vec<_>>())));
    h.set_sh_icon(ModelRc::new(VecModel::from(out.iter().map(|x| x.icon.clone().unwrap_or_default()).collect::<Vec<_>>())));
    h.set_sh_initial(s(out.iter().map(|x| x.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default()).collect()));
    h.set_sh_game(ModelRc::new(VecModel::from(out.iter().map(|x| x.game).collect::<Vec<_>>())));
    // QA B29: when they don't all fit, the last place says "+N more" with the names from that place on
    // (sh-tail[i] = the names of shelves i.. joined)
    let tail: Vec<String> = (0..out.len()).map(|i| out[i..].iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(" · ")).collect();
    h.set_sh_tail(s(tail));
    // three pictures per shelf, flattened (a missing one = an empty image; sh-npics says how many are real)
    h.set_sh_npics(ModelRc::new(VecModel::from(out.iter().map(|x| x.pics.len() as i32).collect::<Vec<_>>())));
    let mut flat: Vec<Image> = vec![];
    for x in &out {
        for k in 0..3 {
            flat.push(x.pics.get(k).cloned().unwrap_or_default());
        }
    }
    h.set_sh_pics(ModelRc::new(VecModel::from(flat)));
}

fn pic(p: &std::path::Path) -> Option<Image> {
    let t = std::fs::metadata(p).and_then(|m| m.modified()).ok()?;
    if let Some((t0, img)) = PICS.with(|c| c.borrow().get(p).cloned()) {
        if t0 == t {
            return img;
        }
    }
    let img = library::decode(p).map(Image::from_rgb8);
    PICS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 64 {
            c.clear();
        }
        c.insert(p.to_path_buf(), (t, img.clone()));
    });
    img
}

/// Home's own actions (main.rs passes on what it doesn't handle). Returns true if Home must be filled again.
pub fn act(a: &AppWindow, lib: &Rc<LibModel>, what: &str) -> bool {
    let (list, new_mode) = LIST.with(|l| l.borrow().clone());
    let pos = CUR_POS.with(|p| p.get());
    let cur = current(lib);
    if matches!(what, "prev" | "next" | "keep") {
        LAST_ACT.with(|t| t.set(Some(std::time::Instant::now())));
    }
    match what {
        "prev" => {
            if pos > 0 && pos - 1 < list.len() {
                set_current(lib, &list, pos - 1);
            }
            true
        }
        "next" | "keep" => {
            let Some(i) = cur else { return false };
            // "keep": main.rs has already starred it (the same toggle as a card's star: toast + sidebar counts)
            mark_seen(lib, i);
            // the next clip: in a list of new clips the next still-new one after this (else from the top); when none
            // is left Home goes back to the latest clip; in the list of all clips simply the next older one
            if new_mode {
                let still = |k: &usize| is_new(&lib.clip(list[*k]));
                match ((pos + 1)..list.len()).find(still).or_else(|| (0..pos).find(still)) {
                    Some(k) => set_current(lib, &list, k),
                    None => {
                        CUR.with(|c| c.borrow_mut().clear());
                        CUR_POS.with(|p| p.set(0));
                    }
                }
            } else if pos + 1 < list.len() {
                set_current(lib, &list, pos + 1);
            }
            true
        }
        "del" => {
            if let Some(i) = cur {
                crate::lib_del::delete_ui(a, lib.clone(), vec![i]);
            }
            false
        }
        _ => false,
    }
}

/// the user did something with the clip on show (sent it, made a link, opened it): it isn't new any more
/// QA B36: a clip played anywhere (Library, player) has been looked at: not new on Home any more
pub fn seen_idx(lib: &LibModel, idx: usize) {
    if idx < lib.len() {
        mark_seen(lib, idx);
    }
}

/// QA E6 (11 Nov 2026): the clip `idx` is about to be rewritten in place (player Trim > Replace): the new file gets a
/// new date, which made an old, already seen clip "NEW SINCE YOU LAST LOOKED" again. Call BEFORE the library reloads
/// it (its old date still decides whether it was seen); a seen clip stays seen under its place in the folder.
pub fn keep_seen(lib: &LibModel, idx: usize) {
    if idx >= lib.len() {
        return;
    }
    let c = lib.clip(idx);
    if !can_be_new(&c) || is_new(&c) {
        return;
    }
    with_seen(|s| {
        s.names.insert(key(&c));
    });
    save_seen();
}

pub fn touched(lib: &LibModel) {
    LAST_ACT.with(|t| t.set(Some(std::time::Instant::now())));
    if let Some(i) = current(lib) {
        mark_seen(lib, i);
    }
}

/// open the library on a shelf's folder
pub fn open_shelf(a: &AppWindow, id: &str) {
    a.invoke_pick_filter(id.into());
}
