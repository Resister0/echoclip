//! Echo Cloud savings (cloud-savings session, 10 Nov 2026, approved by the developer). A child module of recorder.rs,
//! so it can use the recorder's private cloud helpers.
//!
//! - Smaller Echo Cloud copy: what goes up to Echo Cloud is a GPU re-encode (H.264, constant quality, max 1080p,
//!   only the mix track, +faststart). The clip on the PC is never touched. Skipped when it saves less than 15 %.
//!   Always on: the "Original quality in Echo Cloud" switch was removed (William, 10 Nov 2026; an old settings.json
//!   key "cloud_original" is ignored). The user's own clouds get the original.
//! - No duplicate uploads: the clip's SHA-256 goes with the upload; the server says when the same clip is already
//!   stored under another name (a clip renamed / moved outside Echo, or the same clip in two folders).
//! - Share-only uploads: the server deletes them with their link; /keep tells it a clip is backed up on purpose.
//! - /touch: Echo reads Echo Cloud clips straight from R2 (rclone), so it tells the server which clips are in use
//!   (the server's storage class job moves clips nobody watches to cheaper storage).

use super::*;

/// What the Echo Cloud copy needs to save to be worth it (else the original goes up).
const MIN_SAVING: f64 = 0.15;

/// Constant quality per encoder - picked from the measurements in Claude outputs/cloud_savings.md (looks the same
/// as the original at normal viewing). AMF: QVBR level, higher = better. The others: lower = better.
const Q_AMF: &str = "34";
const Q_NVENC: &str = "24";
const Q_QSV: &str = "24";
/// VA-API: constant QP - tuned on Nobara (RX 9070 XT, Mesa radeonsi, 10 Nov 2026, Claude outputs/CLOUD_STORAGE_SIZES.md):
/// SSIM 0.990-0.997 on 6 clips (whole CS2 clip 0.987), like AMF level 34; 24 was 12 % smaller but Minecraft and a
/// busy short clip fell to 0.988. Not QVBR (works with the MAX_RATE cap on radeonsi): same size for the same quality
/// over whole clips, but much softer in the busiest seconds. No cap here (CQP ignores -maxrate); a copy that
/// saves too little is dropped anyway (MIN_SAVING).
const Q_VAAPI: &str = "23";
const Q_X264: &str = "23";
/// cap for very busy scenes (the recorder's peak is about 20 Mbps)
const MAX_RATE: &str = "16M";
const BUF: &str = "32M";

impl Recorder {
    /// The file that goes up to Echo Cloud for `path`, and whether it is a temporary copy (deleted after the upload).
    /// `rel` only names the log lines. Falls back to the clip itself (AV1: its H.264 copy) when converting fails
    /// or doesn't save at least 15 %. `stage` gets ("smaller" | "h264", 0..1) while a copy is made (progress box).
    pub(super) fn cloud_copy(&self, path: &str, rel: &str, stage: &dyn Fn(&str, f64)) -> Result<(String, bool), String> {
        let cancel = || UP_CANCEL.load(Ordering::Relaxed);
        let h264 = |f: f64| stage("h264", f);
        let src = PathBuf::from(path);
        let sm = fs::metadata(&src).map_err(err)?;
        let dir = self.thumbs_dir().join("cloudup");
        fs::create_dir_all(&dir).map_err(err)?;
        // copies left by uploads that were cancelled and never tried again
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            if e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).map_or(false, |d| d > Duration::from_secs(7 * 86400)) {
                let _ = fs::remove_file(e.path());
            }
        }
        // named after the clip's place in the library: two clips with the same name in different folders must never
        // share a copy (the file name in Echo Cloud comes from the clip itself, not from this copy)
        let name = format!("{}.mp4", rel.trim_end_matches(".mp4").trim_end_matches(".MP4").replace(['/', '\\'], "~"));
        let out = dir.join(&name);
        let sig = format!("{}:{}", sm.len(), mtime_secs(&src));
        let no_gain = dir.join(format!("{name}.orig"));
        let copy_sig = dir.join(format!("{name}.sig"));
        // made before (an upload that failed or was cancelled) / known not to be worth it
        if fs::read_to_string(&no_gain).map_or(false, |s| s.trim() == sig) {
            return self.shareable_with(path, &h264, &cancel).map(|p| (p, false));
        }
        if fs::metadata(&out).map_or(false, |m| m.len() > 0) && fs::read_to_string(&copy_sig).map_or(false, |s| s.trim() == sig) {
            return Ok((out.to_string_lossy().to_string(), true));
        }
        let _ = fs::remove_file(&copy_sig);
        // small clips: a few MB either way
        if sm.len() < 3_000_000 {
            return self.shareable_with(path, &h264, &cancel).map(|p| (p, false));
        }
        let height: u32 = hidden("ffprobe").args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=height", "-of", "csv=p=0"])
            .arg(&src).output().ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().lines().next().and_then(|l| l.trim().parse().ok())).unwrap_or(0);
        if height == 0 {
            return self.shareable_with(path, &h264, &cancel).map(|p| (p, false));
        }
        let scale = height > 1080;
        let tmp = dir.join(format!("{name}.tmp"));
        let t0 = Instant::now();
        let dur = self.thumb(path).map(|t| t.duration).unwrap_or(0.0).max(1.0);
        stage("smaller", 0.0);
        let run = |venc: &[&str], vaapi: bool| -> bool {
            if skip_enc(venc) || UP_CANCEL.load(Ordering::Relaxed) { return false; }
            let mut c = ffmpeg();
            c.args(["-hide_banner", "-loglevel", "error", "-progress", "pipe:1", "-nostats", "-y"]);
            if vaapi {
                let Some(d) = vaapi_device() else { return false };
                c.args(["-vaapi_device", &d]);
            }
            // the mix track only (what players hear; the share page plays it too), copied as it is
            // passthrough: every frame keeps its own time (the recorder's clips have uneven frame times; the default
            // made them even by doubling / dropping frames)
            c.arg("-i").arg(&src).args(["-map", "0:v:0", "-map", "0:a:0?", "-c:a", "copy", "-sn", "-dn", "-fps_mode", "passthrough"]);
            let mut vf: Vec<&str> = Vec::new();
            if scale { vf.push("scale=-2:1080:flags=bicubic"); }
            if vaapi { vf.push("format=nv12,hwupload"); }
            if !vf.is_empty() { c.args(["-vf", &vf.join(",")]); }
            c.args(venc).args(["-movflags", "+faststart", "-f", "mp4"]).arg(&tmp).stderr(Stdio::null());
            // "Cancel" in the upload box stops the conversion too; its progress shows in the box (av1-upload-fix)
            ffmpeg_watch(&mut c, dur, &|f| stage("smaller", f), &cancel) && fs::metadata(&tmp).map_or(false, |m| m.len() > 0)
        };
        // the recorder may be using the GPU encoder at the same moment: AMF then sometimes fails at once - try it twice
        let amf = ["-c:v", "h264_amf", "-quality", "balanced", "-rc", "qvbr", "-qvbr_quality_level", Q_AMF, "-maxrate", MAX_RATE, "-bufsize", BUF];
        let ok = run(&amf, false) || run(&amf, false)
            || run(&["-c:v", "h264_nvenc", "-preset", "p5", "-rc", "vbr", "-cq", Q_NVENC, "-b:v", "0", "-maxrate", MAX_RATE, "-bufsize", BUF, "-pix_fmt", "yuv420p"], false)
            || run(&["-c:v", "h264_qsv", "-global_quality", Q_QSV, "-maxrate", MAX_RATE, "-bufsize", BUF], false)
            || (cfg!(target_os = "linux") && run(&["-c:v", "h264_vaapi", "-rc_mode", "CQP", "-qp", Q_VAAPI], true))
            || run(&["-c:v", "libx264", "-preset", "veryfast", "-crf", Q_X264, "-maxrate", MAX_RATE, "-bufsize", BUF, "-pix_fmt", "yuv420p"], false);
        if UP_CANCEL.swap(false, Ordering::Relaxed) {
            let _ = fs::remove_file(&tmp);
            // cancelled by hand: not again on its own (like a cancelled upload)
            log_line(&format!("cloud: upload of {rel} cancelled while making its Echo Cloud copy"));
            let mut sk = UP_SKIPPED.lock().unwrap();
            if !sk.iter().any(|x| x == rel) { sk.push(rel.to_string()); }
            return Err("Upload cancelled".into());
        }
        if !ok {
            let _ = fs::remove_file(&tmp);
            log_line(&format!("echo cloud: couldn't make a smaller copy of {rel} - uploading the clip itself"));
            return self.shareable_with(path, &h264, &cancel).map(|p| (p, false));
        }
        let small = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
        // compared with what would go up otherwise: the clip itself, an AV1 clip as its H.264 copy (about twice the
        // size - shareable() gives H.264 twice AV1's bitrate; not made here just to compare)
        let base = if is_av1(path) { sm.len() * 2 } else { sm.len() };
        if small as f64 > base as f64 * (1.0 - MIN_SAVING) {
            let _ = fs::remove_file(&tmp);
            let _ = fs::write(&no_gain, &sig);
            log_line(&format!("echo cloud: smaller copy of {rel} would save only {:.0} % - uploading the clip itself",
                100.0 - small as f64 * 100.0 / base.max(1) as f64));
            return self.shareable_with(path, &h264, &cancel).map(|p| (p, false));
        }
        let _ = fs::remove_file(&out);
        fs::rename(&tmp, &out).map_err(err)?;
        let _ = fs::write(&copy_sig, &sig);
        log_line(&format!("echo cloud: copy of {rel} for Echo Cloud: {:.1} MB -> {:.1} MB ({:.0} % smaller{}) in {:.0} s",
            base as f64 / 1e6, small as f64 / 1e6, 100.0 - small as f64 * 100.0 / base.max(1) as f64,
            if scale { ", 1080p" } else { "" }, t0.elapsed().as_secs_f32()));
        Ok((out.to_string_lossy().to_string(), true))
    }

    /// SHA-256 of a clip on this PC (hex), cached in .thumbs/hashes.json by size + change time.
    pub(super) fn clip_hash(&self, path: &Path, rel: &str) -> Option<String> {
        use std::io::Read;
        let md = fs::metadata(path).ok()?;
        let sig = format!("{}:{}", md.len(), mtime_secs(path));
        let file = self.thumbs_dir().join("hashes.json");
        let mut m: std::collections::BTreeMap<String, (String, String)> =
            fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if let Some((s, h)) = m.get(rel) {
            if *s == sig { return Some(h.clone()); }
        }
        let t0 = Instant::now();
        let mut f = fs::File::open(path).ok()?;
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).ok()?;
            if n == 0 { break; }
            ctx.update(&buf[..n]);
        }
        let h: String = ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
        if t0.elapsed() > Duration::from_secs(2) { log_line(&format!("echo cloud: fingerprint of {rel} took {:.1} s", t0.elapsed().as_secs_f32())); }
        // forget clips that are gone (the file stays small)
        let root = self.clips_dir();
        m.retain(|r, _| root.join(r).exists());
        m.insert(rel.to_string(), (sig, h.clone()));
        let _ = write_patient(&file, &serde_json::to_string(&m).unwrap_or_default());
        Some(h)
    }

    /// Before uploading `rel` to Echo Cloud at `ec_key`: is the same clip already there under another name?
    /// Some(None) = it was there under a name that's gone from this PC (renamed / moved outside Echo): it was moved
    /// to `ec_key`, nothing to upload. Some(Some(other)) = the same clip is on this PC twice and Echo Cloud has it as
    /// `other`: not stored twice. None = upload it (also: older server, offline).
    pub(super) fn ec_dedup(&self, cfg: &crate::settings::Settings, rel: &str, cp: &str, ec_key: &str, hash: &str) -> Option<Option<String>> {
        let j = ec_call(&ec_api(cfg), "POST", "/upload/check", &cfg.ec_token, Some(serde_json::json!({ "path": ec_key, "hash": hash }))).ok()?;
        let dup = j["dup"].as_str().filter(|d| *d != ec_key)?.to_string();
        let other_cp = dup.strip_prefix("Echo/")?.to_string();
        let other_rel = strip_date(&other_cp);
        let mut st = self.cloud_state();
        let entry = |rel: &str, cp: &str| { let p = Path::new(rel); CloudFile {
            rel: rel.to_string(), path: cp.to_string(),
            name: p.file_stem().unwrap_or_default().to_string_lossy().into(),
            folder: p.parent().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(),
            size: 0, modified: now_unix(), local: true,
        } };
        if other_rel == rel {
            // this clip, in another date folder: Echo didn't know where it was
            st.files.retain(|f| f.rel != rel);
            st.files.push(entry(rel, &other_cp));
            self.save_cloud_state(&st);
            log_line(&format!("echo cloud: {rel} is already in Echo Cloud ({other_cp})"));
            return Some(None);
        }
        // the other name is still on this PC, or the server moved that one here from this very name before
        // (two PCs that each have only one of the names would move it back and forth): keep one copy, as it is
        let moved_back = j["moved_from"].as_str() == Some(ec_key);
        if self.clips_dir().join(&other_rel).exists() || moved_back {
            st.dups.insert(rel.to_string(), other_rel.clone());
            self.save_cloud_state(&st);
            log_line(&format!("echo cloud: {rel} is the same clip as {other_rel} - Echo Cloud keeps one copy"));
            return Some(Some(other_rel));
        }
        // renamed / moved outside Echo: the cloud copy follows instead of a second upload
        let b = serde_json::json!({ "from": dup, "to": ec_key, "dedup": true });
        if let Err(e) = ec_call(&ec_api(cfg), "POST", "/move", &cfg.ec_token, Some(b)) {
            log_line(&format!("echo cloud: couldn't move {other_rel} to {rel} ({e}) - uploading it"));
            return None;
        }
        let size = st.files.iter().find(|f| f.rel == other_rel).map_or(0, |f| f.size);
        st.files.retain(|f| f.rel != other_rel && f.rel != rel);
        let mut e = entry(rel, cp);
        e.size = size;
        st.files.push(e);
        st.keep_remote.remove(&other_rel);
        if st.share_only.remove(&other_rel) { st.share_only.insert(rel.to_string()); }
        if let Some(v) = st.smaller.remove(&other_rel) { st.smaller.insert(rel.to_string(), v); }
        if st.h264.remove(&other_rel) { st.h264.insert(rel.to_string()); }
        self.save_cloud_state(&st);
        // its share link moved with it on the server
        let mut m = self.links();
        if let Some(mut l) = m.remove(&other_rel) {
            if l.key == dup { l.key = ec_key.to_string(); }
            m.insert(rel.to_string(), l);
            self.save_links(&m);
        }
        if self.cloud_serve.lock().unwrap().is_some() { SERVE_STALE.store(true, Ordering::Relaxed); }
        log_line(&format!("echo cloud: {other_rel} was renamed to {rel} - moved in Echo Cloud instead of uploading it again"));
        Some(None)
    }

    /// Tell Echo Cloud these clips are backed up on purpose (they stay when the share link they were uploaded for
    /// expires). Ok also on an older server (it never deletes them anyway).
    pub(super) fn ec_keep(&self, rels: &[String]) -> Result<(), String> {
        if rels.is_empty() { return Ok(()); }
        let cfg = self.settings();
        if cfg.ec_token.is_empty() { return Err("Sign in to Echo Cloud first".into()); }
        let paths: Vec<String> = rels.iter().map(|r| format!("Echo/{}", self.cpath(r).trim_start_matches('/'))).collect();
        match ec_call(&ec_api(&cfg), "POST", "/keep", &cfg.ec_token, Some(serde_json::json!({ "paths": paths }))) {
            Ok(_) => {}
            Err(e) if e.contains("not found") => {}
            Err(e) => return Err(e),
        }
        let mut st = self.cloud_state();
        for r in rels { st.share_only.remove(r); }
        self.save_cloud_state(&st);
        log_line(&format!("echo cloud: {} clip(s) uploaded for a link are kept now", rels.len()));
        Ok(())
    }

    /// A cloud clip is played or downloaded: tell Echo Cloud it's in use (at most once an hour per clip, in the
    /// background). The server keeps clips nobody watches in cheaper storage.
    pub fn cloud_touch(&self, rel: &str) {
        static LAST: Mutex<Option<std::collections::HashMap<String, u64>>> = Mutex::new(None);
        let cfg = self.settings();
        if cfg.ec_token.is_empty() || !cfg.cloud_accounts.iter().any(|a| a.provider == "echocloud") { return; }
        {
            let mut g = LAST.lock().unwrap_or_else(|e| e.into_inner());
            let m = g.get_or_insert_with(Default::default);
            if m.get(rel).map_or(false, |t| now_unix().saturating_sub(*t) < 3600) { return; }
            m.insert(rel.to_string(), now_unix());
        }
        let key = format!("Echo/{}", self.cpath(rel).trim_start_matches('/'));
        std::thread::spawn(move || {
            let _ = ec_call(&ec_api(&cfg), "POST", "/touch", &cfg.ec_token, Some(serde_json::json!({ "paths": [key] })));
        });
    }

    /// The cloud copy of `rel` (size `cloud_size`) is this PC's clip of `local_size` - the same size, or the smaller
    /// Echo Cloud copy Echo made of exactly this file.
    pub(super) fn same_clip(&self, st: &CloudState, rel: &str, cloud_size: u64, local_size: u64) -> bool {
        cloud_size == local_size || st.smaller.get(rel).map_or(false, |[c, l]| *c == cloud_size && *l == local_size)
    }
}

/// a file's change time in unix seconds (0 = unknown)
fn mtime_secs(p: &Path) -> u64 {
    modified(p).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

impl Recorder {
    /// The clip went up to Echo Cloud only for its expiring share link (the server deletes it with the link).
    pub fn cloud_share_only(&self, path: &str) -> bool {
        self.rel_of(path).map_or(false, |rel| self.cloud_state().share_only.contains(&rel))
    }
}

impl Recorder {
    /// The cloud copy of this clip is the smaller Echo Cloud copy (not the original).
    pub fn cloud_smaller(&self, path: &str) -> bool {
        self.rel_of(path).map_or(false, |rel| self.cloud_state().smaller.contains_key(&rel))
    }

    /// What the cloud holds of this clip: "smaller" (Echo Cloud's smaller copy), "h264" (an AV1 clip's H.264 copy,
    /// plays everywhere) or "" (the clip itself). (av1-upload-fix, 10 Nov 2026)
    pub fn cloud_copy_kind(&self, path: &str) -> &'static str {
        let Some(rel) = self.rel_of(path) else { return "" };
        let st = self.cloud_state();
        if st.h264.contains(&rel) { "h264" } else if st.smaller.contains_key(&rel) { "smaller" } else { "" }
    }

    /// "Free up space" asks first when the cloud's copy isn't the clip itself: (kind as in cloud_copy_kind(), the
    /// cloud copy's size, the clip's size on this PC). An AV1 clip uploaded before Echo remembered its copies counts
    /// as "h264" when the cloud's size differs (cloud_remove_local() checks that copy before deleting anything).
    pub fn cloud_copy_info(&self, path: &str) -> (&'static str, u64, u64) {
        let Some(rel) = self.rel_of(path) else { return ("", 0, 0) };
        let local = fs::metadata(path).map_or(0, |m| m.len());
        let st = self.cloud_state();
        if let Some([c, l]) = st.smaller.get(&rel) {
            if *l == local { return (if st.h264.contains(&rel) { "h264" } else { "smaller" }, *c, local); }
        }
        match st.files.iter().find(|f| f.rel == rel) {
            Some(f) if f.size > 0 && f.size != local && is_av1(path) => ("h264", f.size, local),
            _ => ("", 0, local),
        }
    }

    /// av1-upload-fix (William, 10 Nov 2026): does a share link for this clip need an H.264 share copy - would it
    /// otherwise play an AV1 file (many phones and browsers can't)? Decided by what the cloud really stores:
    /// - not in a cloud yet: the clip on this PC (the link uploads from here)
    /// - Echo's own copy in the cloud (Echo Cloud's smaller copy, an AV1 clip's H.264 copy - "smaller"): never
    /// - the cloud has this very file (same size as on this PC): the clip on this PC
    /// - cloud-only, or a different file in the cloud (old H.264 upload Echo didn't remember): the cloud's file itself
    pub(super) fn link_needs_h264(&self, rel: &str, local: &Path) -> bool {
        let st = self.cloud_state();
        let here = local.exists();
        let local_av1 = || here && is_av1(&local.to_string_lossy());
        let Some(f) = st.files.iter().find(|f| f.rel == rel) else { return local_av1() };
        if st.smaller.contains_key(rel) { return false; }
        if here && fs::metadata(local).map_or(false, |m| m.len() == f.size) { return local_av1(); }
        match self.cloud_probe_av1(rel) {
            Some(v) => v,
            None => { log_line(&format!("cloud: couldn't tell the codec of {rel} in the cloud - going by the clip on this PC")); local_av1() }
        }
    }

    /// Is the cloud's file for `rel` AV1? Asked over the local streaming server (rclone serve, reads only the
    /// header). None = couldn't tell.
    fn cloud_probe_av1(&self, rel: &str) -> Option<bool> {
        let base = self.cloud_stream_base().ok()?;
        let enc = |seg: &str| seg.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
        let url = format!("{}/{}", base.trim_end_matches('/'), self.cpath(rel).trim_start_matches('/').split('/').map(enc).collect::<Vec<_>>().join("/"));
        let o = hidden("ffprobe").args(["-v", "error", "-rw_timeout", "20000000", "-select_streams", "v:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"])
            .arg(&url).output().ok()?;
        let c = String::from_utf8_lossy(&o.stdout).trim().lines().next().unwrap_or("").to_string();
        log_line(&format!("cloud: {rel} in the cloud is {}", if c.is_empty() { "?" } else { &c }));
        (!c.is_empty()).then(|| c == "av1")
    }

    /// The clip's file from the cloud into .thumbs/h264/ - only to make its share copy (deleted afterwards). Shows
    /// "Downloading from <cloud> to make a share copy… N%"; `progress` gets `at`..`at + span`.
    pub(super) fn fetch_for_share(&self, rel: &str, lp: &LinkProg, progress: &dyn Fn(f64), at: f64, span: f64) -> Result<PathBuf, String> {
        let dir = self.thumbs_dir().join("h264");
        fs::create_dir_all(&dir).map_err(err)?;
        let tmp = dir.join(format!("dl~{}", h264_cache_name(Some(rel), "")));
        let t0 = Instant::now();
        lp.stage("fetch", 0.0);
        progress(at);
        let r = rclone_progress(&["copyto", &remote(&self.cpath(rel)), &tmp.to_string_lossy()], |b, t| {
            let f = b as f64 / t.max(1) as f64;
            lp.stage("fetch", f);
            progress(at + f.min(1.0) * span);
        });
        if let Err(e) = r {
            let _ = fs::remove_file(&tmp);
            if UP_CANCEL.load(Ordering::Relaxed) { return Err("Upload cancelled".into()); }
            log_line(&format!("cloud: couldn't fetch {rel} for its share copy: {e}"));
            return Err(format!("Couldn't download this clip from the cloud to make its share copy - check the connection and try again ({e})"));
        }
        if fs::metadata(&tmp).map_or(true, |m| m.len() == 0) {
            let _ = fs::remove_file(&tmp);
            return Err("Couldn't download this clip from the cloud to make its share copy - try again".into());
        }
        log_line(&format!("cloud: fetched {rel} for its share copy in {:.0}s", t0.elapsed().as_secs_f32()));
        Ok(tmp)
    }

    /// av1-upload-fix (William, 10 Nov 2026): a share link for an AV1 clip on the user's own clouds (no Echo Cloud).
    /// The clip itself stays the original there; the link plays an H.264 copy, uploaded as
    /// Echo/.shared/<folder>/<name>.mp4 (dot folders aren't clips in the listing). Made from the clip on this PC
    /// (kept in .thumbs/h264), or - cloud-only clip - from its file fetched from the cloud just for this. None = no
    /// cloud that makes links (only Filen): the caller links the clip itself.
    pub(super) fn link_via_own_copy(&self, rel: &str, local: &Path, days: u32, progress: &dyn Fn(f64)) -> Result<Option<(String, String, u64, serde_json::Value)>, String> {
        let cfg = self.settings();
        let links = |a: &&crate::settings::CloudAccount| a.provider != "echocloud" && a.provider != "filen";
        let Some(acc) = cfg.cloud_accounts.iter().filter(links).find(|a| a.id.to_string() == cfg.cloud_target)
            .or_else(|| cfg.cloud_accounts.iter().find(links)).cloned() else { return Ok(None) };
        let dest = cloud_name(&acc.provider).to_string();
        // the link box / progress box say what's going on (cloud.rs job(), cloud_live.rs)
        let lp = LinkProg::begin(rel, &dest);
        let cancel = || UP_CANCEL.load(Ordering::Relaxed);
        let here = local.exists() && is_av1(&local.to_string_lossy());
        // (progress: [fetch 0-30 %] convert, then upload up to 95 %)
        let (copy, temp) = if here {
            (PathBuf::from(self.shareable_with(&local.to_string_lossy(), &|f| { lp.stage("h264", f); progress(f * 0.5); }, &cancel)?), false)
        } else {
            let got = self.fetch_for_share(rel, &lp, progress, 0.0, 0.3)?;
            let out = got.with_file_name(format!("share~{}", h264_cache_name(Some(rel), "")));
            let r = self.make_h264(&got, &out, 0.0, &|f| { lp.stage("h264", f); progress(0.3 + f * 0.3); }, &cancel);
            let _ = fs::remove_file(&got);
            r?;
            (out, true)
        };
        let at = if here { 0.5 } else { 0.6 };
        let size = fs::metadata(&copy).map_or(0, |m| m.len());
        lp.stage("send", 0.0);
        lp.bytes(0, size);
        let dst = acc_path(&acc, &format!("Echo/.shared/{}", rel.trim_start_matches('/')));
        let t0 = Instant::now();
        // the same copy is there already (a new lifetime for a link made before): not again
        let there = rclone_out(&["lsjson", &dst]).ok().and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
            .and_then(|v| v.first().and_then(|f| f["Size"].as_u64())) == Some(size);
        let r = if there { Ok(()) } else { rclone_progress(&["copyto", &copy.to_string_lossy(), &dst], |b, t| {
            progress(at + b as f64 / t.max(1) as f64 * (0.95 - at));
            lp.bytes(b, t);
        }) };
        if temp { let _ = fs::remove_file(&copy); }
        if UP_CANCEL.load(Ordering::Relaxed) && r.is_err() { return Err("Upload cancelled".into()); }
        r?;
        log_line(&format!("cloud: H.264 share copy of {rel} ({:.1} MB{}) to {dest} in {:.0}s", size as f64 / 1e6,
            if temp { ", made from the cloud's file" } else { "" }, t0.elapsed().as_secs_f32()));
        let exp = format!("{days}d");
        let mut args = vec!["link"];
        if days > 0 { args.extend(["--expire", exp.as_str()]); }
        args.push(dst.as_str());
        let url = rclone_out(&args).map_err(|e| format!("Couldn't make a link: {e}"))?.trim().lines().last().unwrap_or("").to_string();
        if !url.starts_with("http") { return Err("This cloud didn't return a link".into()); }
        log_line(&format!("cloud: link for {rel} (H.264 share copy in {dest})"));
        let expires = if days > 0 { now_unix() + days as u64 * 86400 } else { 0 };
        // remembered with the link (links.json "own"): "Stop sharing" / expiry / deleting the clip delete this copy
        Ok(Some((url, String::new(), expires, serde_json::json!({ "own_copy": dst }))))
    }

    /// av1-upload-fix (10 Nov 2026): a link for a clip that's already playable in the user's own cloud (not AV1)
    /// plays a share copy next to it - Echo/.shared/<folder>/<name>.mp4, copied inside that cloud (nothing downloaded
    /// or uploaded) - so "Stop sharing", expiry and deleting the clip can end the link by deleting the copy. None =
    /// the cloud can't copy by itself (or the copy failed): the caller links the clip itself, as before.
    pub(super) fn link_via_server_copy(&self, rel: &str, acc: &crate::settings::CloudAccount, src: &str, days: u32, progress: &dyn Fn(f64)) -> Result<Option<(String, String, u64, serde_json::Value)>, String> {
        if acc.provider == "filen" || !backend_copies(acc) {
            log_line(&format!("cloud: {} can't copy inside the cloud - linking {rel} itself", cloud_name(&acc.provider)));
            return Ok(None);
        }
        let size_of = |p: &str| rclone_out(&["lsjson", p]).ok().and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
            .and_then(|v| v.first().and_then(|f| f["Size"].as_u64()));
        let Some(size) = size_of(src) else { return Ok(None) };
        let dst = acc_path(acc, &format!("Echo/.shared/{}", rel.trim_start_matches('/')));
        let t0 = Instant::now();
        progress(0.2);
        // made before (a new lifetime for the same clip): not again
        if size_of(&dst) != Some(size) {
            if let Err(e) = rclone_out(&["copyto", src, &dst]) {
                log_line(&format!("cloud: couldn't copy {rel} inside {} for its link ({e}) - linking the clip itself", cloud_name(&acc.provider)));
                return Ok(None);
            }
            if size_of(&dst) != Some(size) {
                let _ = own_copy_delete(&dst);
                log_line(&format!("cloud: the share copy of {rel} in {} came out wrong - linking the clip itself", cloud_name(&acc.provider)));
                return Ok(None);
            }
            log_line(&format!("cloud: share copy of {rel} made inside {} in {:.1}s ({:.1} MB)", cloud_name(&acc.provider), t0.elapsed().as_secs_f32(), size as f64 / 1e6));
        }
        progress(0.8);
        let exp = format!("{days}d");
        let mut args = vec!["link"];
        if days > 0 { args.extend(["--expire", exp.as_str()]); }
        args.push(dst.as_str());
        let url = match rclone_out(&args) {
            Ok(o) => o.trim().lines().last().unwrap_or("").to_string(),
            Err(e) => { let _ = own_copy_delete(&dst); return Err(format!("Couldn't make a link: {e}")); }
        };
        if !url.starts_with("http") { let _ = own_copy_delete(&dst); return Err("This cloud didn't return a link".into()); }
        log_line(&format!("cloud: link for {rel} (share copy in {})", cloud_name(&acc.provider)));
        let expires = if days > 0 { now_unix() + days as u64 * 86400 } else { 0 };
        Ok(Some((url, String::new(), expires, serde_json::json!({ "own_copy": dst }))))
    }

    /// The user's own cloud that holds this clip (for "can't be stopped from Echo" texts).
    pub(super) fn own_link_cloud(&self, rel: &str) -> String {
        let cfg = self.settings();
        let own: Vec<_> = cfg.cloud_accounts.iter().filter(|a| a.provider != "echocloud").collect();
        if own.len() == 1 { return cloud_name(&own[0].provider).to_string(); }
        let cp = self.cpath(rel);
        own.iter().find(|a| rclone_out(&["lsjson", &acc_path(a, &format!("Echo/{}", cp.trim_start_matches('/')))]).map_or(false, |j| j.contains("\"Path\"")))
            .map_or("Your cloud".to_string(), |a| cloud_name(&a.provider).to_string())
    }

    /// The share copy in the user's own cloud, deleted in the background (the link stops working).
    pub(super) fn drop_own_copy(&self, path: &str) {
        let p = path.to_string();
        std::thread::spawn(move || { let _ = own_copy_delete(&p); });
    }

    /// Background sync: own-cloud links that expired (Google Drive's links don't expire by themselves - Echo
    /// remembers the lifetime): their share copies are deleted, the links forgotten.
    pub(super) fn sweep_own_share_copies(&self) {
        let now = now_unix();
        let mut m = self.links();
        let gone: Vec<String> = m.iter().filter(|(_, e)| !e.own.is_empty() && e.expires != 0 && e.expires <= now).map(|(k, _)| k.clone()).collect();
        if gone.is_empty() { return; }
        let paths: Vec<String> = gone.iter().filter_map(|k| m.remove(k)).map(|e| e.own).collect();
        self.save_links(&m);
        for p in paths { let _ = own_copy_delete(&p); }
    }

    /// av1-upload-fix: an AV1 clip uploaded before Echo remembered its copies (10 Nov 2026) went up as its H.264
    /// copy, so the cloud's size isn't the clip's. That copy is still on this PC (.thumbs/h264/, under the clip's
    /// file name then): the same size as the cloud file and the same length as the clip = the cloud has the whole
    /// clip. Remembered from then on.
    pub(super) fn h264_copy_matches(&self, path: &str, rel: &str, cloud_size: u64, local_size: u64) -> bool {
        if !is_av1(path) { return false; }
        let dir = self.thumbs_dir().join("h264");
        let file = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let len = |p: &Path| hidden("ffprobe").args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"]).arg(p)
            .output().ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok());
        let ok = [dir.join(h264_cache_name(Some(rel), &file)), dir.join(&file)].iter().any(|c| {
            fs::metadata(c).map_or(false, |m| m.len() == cloud_size)
                && matches!((len(c), len(Path::new(path))), (Some(a), Some(b)) if (a - b).abs() < 0.5)
        });
        if ok {
            let mut st = self.cloud_state();
            st.smaller.insert(rel.to_string(), [cloud_size, local_size]);
            st.h264.insert(rel.to_string());
            self.save_cloud_state(&st);
            log_line(&format!("cloud: {rel} - the cloud has its H.264 copy (uploaded before Echo remembered it)"));
        }
        ok
    }
}

/// av1-upload-fix: a share link's copy shows in the progress boxes through UP_PROGRESS (rel + stage: "fetch" /
/// "h264" / "convert" / "send"). Cleared - and a Cancel reset - when it goes out of scope.
pub(super) struct LinkProg { rel: String }

impl LinkProg {
    pub(super) fn begin(rel: &str, dest: &str) -> LinkProg {
        UP_CANCEL.store(false, Ordering::Relaxed);
        let mut p = UP_PROGRESS.lock().unwrap();
        if p.count == 0 { p.idx = 1; p.count = 1; }
        p.rel = rel.to_string(); p.got = 0; p.total = 0; p.dest = dest.to_string(); p.stage.clear(); p.stage_f = 0.0;
        LinkProg { rel: rel.to_string() }
    }
    pub(super) fn stage(&self, s: &str, f: f64) {
        let mut p = UP_PROGRESS.lock().unwrap();
        if p.rel == self.rel { if p.stage != s { p.stage = s.to_string(); } p.stage_f = f as f32; }
    }
    pub(super) fn bytes(&self, b: u64, t: u64) {
        let mut p = UP_PROGRESS.lock().unwrap();
        if p.rel == self.rel { p.got = b; if t > 0 { p.total = t; } }
    }
}

impl Drop for LinkProg {
    fn drop(&mut self) {
        let mut p = UP_PROGRESS.lock().unwrap();
        if p.rel == self.rel { p.rel.clear(); p.stage.clear(); if p.idx >= p.count { p.idx = 0; p.count = 0; } }
        UP_CANCEL.store(false, Ordering::Relaxed);
    }
}

/// Delete a share copy in the user's own cloud (rclone path). A copy that's already gone counts as done.
pub(super) fn own_copy_delete(path: &str) -> Result<(), String> {
    match rclone_out(&["deletefile", "--retries", "1", path]) {
        Ok(_) => { log_line(&format!("cloud: deleted share copy {path}")); Ok(()) }
        Err(e) if e.contains("not found") || e.contains("doesn't exist") || e.contains("directory not found") => Ok(()),
        Err(e) => { log_line(&format!("cloud: couldn't delete share copy {path}: {e}")); Err(format!("Couldn't delete the share copy in the cloud - try again ({e})")) }
    }
}

/// Can this cloud copy a file inside itself (Google Drive, OneDrive, Dropbox, pCloud, S3, a folder…)? From rclone's
/// own feature list for the account ("Copy"), asked once per account per run.
fn backend_copies(acc: &crate::settings::CloudAccount) -> bool {
    static SEEN: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());
    let key = acc_path(acc, "Echo");
    if let Some((_, v)) = SEEN.lock().unwrap().iter().find(|(k, _)| *k == key) { return *v; }
    // couldn't ask (offline…): not remembered, asked again next time
    let Some(v) = rclone_out(&["backend", "features", &key]).ok()
        .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
        .map(|j| j["Features"]["Copy"].as_bool().unwrap_or(false)) else { return false };
    log_line(&format!("cloud: {} copies inside the cloud: {v}", cloud_name(&acc.provider)));
    SEEN.lock().unwrap().push((key, v));
    v
}
