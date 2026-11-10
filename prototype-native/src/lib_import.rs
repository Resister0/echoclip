//! Chat D - Import REAL (approved): port of Echo's recorder.rs import_folder / import_one.
//! Copies every video under a folder into the clips folder (originals stay), one Echo folder per game folder,
//! keeps the file dates (the library sorts by them), skips what Echo already imported (.thumbs/imported.json) and
//! files deleted in the meantime, checks there is room first (everything + 5 GB), converts what Echo can't play
//! (not H.264 / full-range colours) with NVENC -> AMF -> QSV -> libx264, and makes the thumbnail + .dur right away.
//! Runs on a worker thread with a progress box (Cancel stops after the current file).
use crate::AppWindow;
use slint::ComponentHandle;
use std::collections::HashSet;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const VIDEO_EXTS: [&str; 12] = ["mp4", "mov", "m4v", "mkv", "webm", "avi", "ts", "flv", "wmv", "mpg", "mpeg", "3gp"];
/// Echo's TV_YUV + TV_TAGS: BT.709 TV range (Discord / players show full range too dark)
const TV_YUV: &str = "scale=out_range=tv:out_color_matrix=bt709,format=yuv420p";
const TV_TAGS: [&str; 8] = ["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];

thread_local! {
    static RUNNING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn hidden(p: &Path) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(p);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.stdin(Stdio::null());
    c
}

/// Medal keeps clips in <capture folder>\Clips
fn import_root(p: &Path) -> PathBuf {
    let c = p.join("Clips");
    if c.is_dir() {
        c
    } else {
        p.to_path_buf()
    }
}

fn collect(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let n = e.file_name().to_string_lossy().to_lowercase();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if n.starts_with('.') || ["thumbnails", "thumbs", "editor", "cache", "temp", "tmp", "screenshots", "logs", "shared clips"].contains(&n.as_str()) {
                continue;
            }
            collect(&p, depth + 1, out);
        } else {
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if VIDEO_EXTS.contains(&ext.as_str()) && !n.contains(".tmp") && !n.contains(".part") {
                out.push(p);
            }
        }
    }
}

/// free bytes on the drive of `dir` (0 = unknown)
fn disk_free(dir: &Path) -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let w: Vec<u16> = dir.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();
        let mut free: u64 = 0;
        let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
        if ok != 0 {
            return free;
        }
        0
    }
    #[cfg(not(windows))]
    {
        Command::new("df")
            .args(["-B1", "--output=avail"])
            .arg(dir)
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().nth(1).and_then(|l| l.trim().parse().ok()))
            .unwrap_or(0)
    }
}

fn probe(ff: &Path, src: &Path, args: &[&str]) -> String {
    hidden(&crate::edrender::ffprobe(ff)).args(["-v", "error"]).args(args).arg(src).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

/// one ffmpeg run, `-progress pipe:1`: tick(seconds done). False = failed or cancelled.
fn run_ff(mut c: Command, tick: &dyn Fn(f64), stop: &AtomicBool) -> bool {
    c.stdout(Stdio::piped()).stderr(Stdio::null());
    let Ok(mut ch) = c.spawn() else { return false };
    if let Some(out) = ch.stdout.take() {
        for l in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(us) = l.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
                tick(us / 1e6);
            }
            if stop.load(Ordering::Relaxed) {
                let _ = ch.kill();
                break;
            }
        }
    }
    ch.wait().is_ok_and(|s| s.success()) && !stop.load(Ordering::Relaxed)
}

/// Echo's import_one: copy (H.264 + AAC/MP3, TV range) or convert into `dir`, keep the date
fn import_one(ff: &Path, src: &Path, dir: &Path, prog: &dyn Fn(f64), stop: &AtomicBool) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stem = src.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let out = (1..).map(|n| dir.join(if n == 1 { format!("{stem}.mp4") } else { format!("{stem} ({n}).mp4") })).find(|p| !p.exists()).unwrap();
    let tmp = dir.join(format!("{stem}.{}.importing.tmp", std::process::id()));
    let first = |s: String| s.lines().next().unwrap_or("").trim().to_string();
    let vc = first(probe(ff, src, &["-select_streams", "v:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"]));
    let ac = first(probe(ff, src, &["-select_streams", "a:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"]));
    if vc.is_empty() {
        return Err("no video stream".into());
    }
    let dur = probe(ff, src, &["-show_entries", "format=duration", "-of", "csv=p=0"]).trim().parse::<f64>().unwrap_or(0.0).max(0.1);
    let vinfo = probe(ff, src, &["-select_streams", "v:0", "-show_entries", "stream=pix_fmt,color_range", "-of", "csv=p=0"]);
    // full range (Medal, phones, some OBS setups) -> real TV range. (Echo also samples the picture for untagged full
    // range - the prototype trusts the tags.)
    let full = vinfo.contains("pc") || vinfo.contains("yuvj");
    let copy_ok = vc == "h264" && !full && vinfo.trim().starts_with("yuv420p") && (ac.is_empty() || ac == "aac" || ac == "mp3");
    let vf = if full { "scale=in_range=pc:out_range=tv:out_color_matrix=bt709,format=yuv420p" } else { TV_YUV };
    let tick = |t: f64| prog(t / dur);
    let run = |args: &[&str]| -> bool {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let mut c = hidden(ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(src).args(["-map", "0:v:0", "-map", "0:a?"]).args(args);
        if !args.contains(&"copy") {
            c.args(["-vf", vf]).args(TV_TAGS);
        }
        c.args(["-progress", "pipe:1", "-nostats", "-movflags", "+faststart", "-f", "mp4"]).arg(&tmp);
        run_ff(c, &tick, stop) && std::fs::metadata(&tmp).is_ok_and(|m| m.len() > 0)
    };
    let aenc = ["-c:a", "aac", "-b:a", "192k", "-ac", "2"];
    let enc = |v: &[&str]| -> Vec<String> { v.iter().chain(aenc.iter()).map(|s| s.to_string()).collect() };
    let tries: Vec<Vec<String>> = vec![
        enc(&["-c:v", "h264_nvenc", "-preset", "p5", "-cq", "22"]),
        enc(&["-c:v", "h264_amf", "-quality", "quality", "-rc", "qvbr", "-qvbr_quality_level", "22"]),
        enc(&["-c:v", "h264_qsv", "-global_quality", "22"]),
        enc(&["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p"]),
    ];
    let mut done = copy_ok && run(&["-c", "copy"]);
    for t in &tries {
        if done || stop.load(Ordering::Relaxed) {
            break;
        }
        let r: Vec<&str> = t.iter().map(|s| s.as_str()).collect();
        done = run(&r);
    }
    if done && std::fs::rename(&tmp, &out).is_ok() {
        // the date it was recorded, not the import time
        if let (Ok(t), Ok(f)) = (std::fs::metadata(src).and_then(|m| m.modified()), std::fs::File::options().write(true).open(&out)) {
            let _ = f.set_modified(t);
        }
        eprintln!("import: {} -> {} ({vc}/{ac}{}{})", src.display(), out.display(), if copy_ok { ", copied" } else { "" }, if full { ", full range -> tv" } else { "" });
        Ok(out)
    } else {
        let _ = std::fs::remove_file(&tmp);
        Err(if stop.load(Ordering::Relaxed) { "cancelled".into() } else { "could not convert".into() })
    }
}

/// Echo's thumb(): <stem>.jpg (480 px, at 0.5 s) + <stem>.dur in .thumbs
fn make_thumb(ff: &Path, clip: &Path, thumbs: &Path) {
    let stem = clip.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let _ = std::fs::create_dir_all(thumbs);
    let img = thumbs.join(format!("{stem}.jpg"));
    if !img.exists() {
        let _ = hidden(ff)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "0.5", "-i"])
            .arg(clip)
            .args(["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"])
            .arg(&img)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let d = probe(ff, clip, &["-show_entries", "format=duration", "-of", "default=nw=1:nk=1"]).trim().parse::<f64>().unwrap_or(0.0);
    if d > 0.0 {
        let _ = std::fs::write(thumbs.join(format!("{stem}.dur")), d.to_string());
    }
}

enum Msg {
    Prog(f32, String),
    Done(usize, usize, Vec<String>),
}

/// start importing everything under `root` (Settings > "Import" on an app line, or "Choose folder…")
pub fn start(a: &AppWindow, root: PathBuf, clips: PathBuf) {
    if RUNNING.with(|r| r.get()) {
        crate::toast(a, "An import is already running");
        return;
    }
    let Some(ff) = crate::sheets::ffmpeg() else {
        crate::toast(a, "Importing needs ffmpeg (Echo downloads it - start Echo once)");
        return;
    };
    let root = import_root(&root);
    if !root.is_dir() {
        crate::toast(a, "That folder isn't there");
        return;
    }
    if root.starts_with(&clips) || clips.starts_with(&root) {
        crate::toast(a, "That's your clips folder (or holds it) - pick the other app's folder");
        return;
    }
    RUNNING.with(|r| r.set(true));
    let id = crate::prog_box("Importing clips…");
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        crate::prog_btn(id, "Cancel", move || stop.store(true, Ordering::Relaxed));
    }
    // game folders: settings.json game_folders (default on) + folder_alias (Echo's game_folder)
    let s = crate::library::echo_settings();
    let game_folders = s.as_ref().and_then(|v| v.get("game_folders")).and_then(|v| v.as_bool()).unwrap_or(true);
    let alias: Vec<(String, String)> = s
        .as_ref()
        .and_then(|v| v.get("folder_alias"))
        .and_then(|v| v.as_object())
        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
        .unwrap_or_default();
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    std::thread::spawn(move || {
        let thumbs = clips.join(".thumbs");
        let mut files = vec![];
        collect(&root, 0, &mut files);
        files.retain(|f| !f.starts_with(&clips));
        files.sort_by_key(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok());
        let list = thumbs.join("imported.json");
        let mut seen: HashSet<String> = std::fs::read_to_string(&list).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let key = |f: &Path| format!("{}|{}", f.to_string_lossy().to_lowercase(), std::fs::metadata(f).map(|m| m.len()).unwrap_or(0));
        let (mut ok, mut skipped, mut bad) = (0usize, 0usize, Vec::<String>::new());
        let need: u64 = files.iter().filter(|f| !seen.contains(&key(f))).filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
        let free = disk_free(&clips);
        if free > 0 && need + 5_000_000_000 > free {
            let gb = |b: u64| b as f64 / 1e9;
            bad.push(format!("Not enough space: these clips need {:.0} GB but only {:.0} GB is free on the clips drive. Free up space or pick a clips folder on a bigger drive (Settings > Clips & Storage).", gb(need), gb(free)));
            let _ = tx.send(Msg::Done(0, 0, bad));
            return;
        }
        let total = files.len();
        for (i, f) in files.iter().enumerate() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let name = f.file_name().unwrap_or_default().to_string_lossy().to_string();
            // deleted in the other app since the list was made: skipped, not an error
            if !f.is_file() {
                skipped += 1;
                continue;
            }
            let k = key(f);
            if seen.contains(&k) {
                skipped += 1;
                continue;
            }
            // first folder under the root = the game (Medal: Clips\<Game>\…, Moments: <Game>\…)
            let rel = f.strip_prefix(&root).unwrap_or(f);
            let game = rel
                .components()
                .rev()
                .skip(1)
                .last()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .filter(|g| !["clips", "videos", "recordings", "captures", "moments", "exported clips", "exports", "exported", "highlights"].contains(&g.to_lowercase().as_str()))
                .unwrap_or_default();
            let mut folder = if game.is_empty() || !game_folders { String::new() } else { crate::library::clean_folder(&game) };
            for _ in 0..8 {
                match alias.iter().find(|(k, _)| k.eq_ignore_ascii_case(&folder)) {
                    Some((_, v)) if !v.eq_ignore_ascii_case(&folder) && !folder.is_empty() => folder = v.clone(),
                    _ => break,
                }
            }
            if crate::library::folder_reserved(&folder) {
                folder.clear();
            }
            let dir = if folder.is_empty() { clips.clone() } else { clips.join(&folder) };
            let txp = tx.clone();
            let label = format!("{} of {total} · {name}", i + 1);
            let each = move |frac: f64| {
                let _ = txp.send(Msg::Prog(((i as f64 + frac.clamp(0.0, 1.0)) / total.max(1) as f64) as f32, label.clone()));
            };
            each(0.0);
            match import_one(&ff, f, &dir, &each, &stop) {
                Ok(out) => {
                    make_thumb(&ff, &out, &thumbs);
                    ok += 1;
                    seen.insert(k);
                    if ok % 10 == 0 {
                        let _ = std::fs::create_dir_all(&thumbs);
                        let _ = std::fs::write(&list, serde_json::to_string(&seen).unwrap_or_default());
                    }
                }
                Err(e) if e == "cancelled" => break,
                Err(e) => bad.push(format!("{name}: {e}")),
            }
        }
        let _ = std::fs::create_dir_all(&thumbs);
        let _ = std::fs::write(&list, serde_json::to_string(&seen).unwrap_or_default());
        eprintln!("import: {total} from {} ({ok} new, {skipped} already there or gone, {} failed)", root.display(), bad.len());
        let _ = tx.send(Msg::Done(ok, skipped, bad));
    });
    // progress back on the UI thread
    let w = a.as_weak();
    let t = Rc::new(slint::Timer::default());
    let t2 = t.clone();
    t.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(120), move || {
        let _keep = &t2;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Prog(p, text) => crate::prog_set(id, None, Some(&text), Some(p * 100.0)), // the box wants 0-100
                Msg::Done(ok, skipped, bad) => {
                    RUNNING.with(|r| r.set(false));
                    let title = if ok == 0 && bad.is_empty() {
                        if skipped > 0 { "Nothing new to import".to_string() } else { "No videos in that folder".to_string() }
                    } else {
                        format!("Imported {ok} clip{}", if ok == 1 { "" } else { "s" })
                    };
                    let text = match bad.len() {
                        0 if skipped > 0 && ok > 0 => format!("{skipped} were already in Echo"),
                        0 => String::new(),
                        1 => bad[0].clone(),
                        n => format!("{n} couldn't be imported - {}", bad[0]),
                    };
                    crate::prog_set(id, Some(&title), Some(&text), Some(if ok > 0 { 100.0 } else { -1.0 }));
                    crate::prog_done(id, if bad.is_empty() { 4000 } else { 9000 });
                    if let Some(a) = w.upgrade() {
                        // the library picks the new files up (sync_disk); the Settings lines re-count
                        let _ = &a;
                        crate::UI_SYNC.with(|f| {
                            if let Some(f) = f.borrow().as_ref() {
                                f(true)
                            }
                        });
                    }
                    t2.stop();
                    return;
                }
            }
        }
    });
}

use std::rc::Rc;
