//! Echo Cloud savings (cloud-savings session, 10 Nov 2026, approved by the developer). A child module of recorder.rs,
//! so it can use the recorder's private cloud helpers.
//!
//! - Smaller Echo Cloud copy: what goes up to Echo Cloud is a GPU re-encode (H.264, constant quality, max 1080p,
//!   only the mix track, +faststart). The clip on the PC is never touched. Skipped when it saves less than 15 %.
//!   Always on: the "Original quality in Echo Cloud" switch was removed (10 Nov 2026; an old settings.json
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
/// as the original at normal viewing). Lower = better.
/// AMF: constant QP for I / P / B frames (faster-share-links, 11 Nov 2026, Claude outputs/faster-share-links.md). Was
/// QVBR level 34: QVBR needs AMF's pre-analysis, which made the encode about twice as slow. QP 26 gave the same or a
/// smaller file with a higher VMAF on every test clip (RX 9070 XT). No cap (CQP ignores -maxrate); a copy that saves
/// too little is dropped anyway (MIN_SAVING).
const QP_AMF: [&str; 3] = ["26", "28", "30"];
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

/// Width and height of a clip's video (0, 0 when ffprobe can't read it).
fn probe_size(src: &Path) -> (u32, u32) {
    let o = hidden("ffprobe").args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(src).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let mut it = o.trim().lines().next().unwrap_or("").split(',').map(|v| v.trim().parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// ffmpeg input options: decode on the AMD GPU and keep the frames there for h264_amf (faster-share-links,
/// 11 Nov 2026). -extra_hw_frames: AMF holds frames while it works; without them the decoder's pool ran out
/// ("Static surface pool size exceeded", ffmpeg stops with an error -> the CPU path takes over).
pub(super) const AMF_DEC: [&str; 6] = ["-hwaccel", "amf", "-hwaccel_output_format", "amf", "-extra_hw_frames", "24"];

/// The clip's video is TV range + BT.709 (Echo's own recordings): no colour conversion needed.
pub(super) fn tv_bt709(src: &Path) -> bool {
    hidden("ffprobe").args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=color_range,color_space", "-of", "csv=p=0"])
        .arg(src).output().map_or(false, |o| String::from_utf8_lossy(&o.stdout).trim() == "tv,bt709")
}

/// Length of a file's video stream in seconds (0 when ffprobe can't tell).
pub(super) fn video_secs(p: &Path) -> f64 {
    hidden("ffprobe").args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=duration", "-of", "csv=p=0"])
        .arg(p).output().ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().lines().next().and_then(|l| l.trim().parse().ok()))
        .unwrap_or(0.0)
}

impl Recorder {
    /// The file that goes up to Echo Cloud for `path`, and whether it is a temporary copy (deleted after the upload).
    /// `rel` only names the log lines. Falls back to the clip itself (AV1: its H.264 copy) when converting fails
    /// or doesn't save at least 15 %. `stage` gets ("smaller" | "h264", 0..1) while a copy is made (progress box).
    pub(super) fn cloud_copy(&self, path: &str, rel: &str, stage: &dyn Fn(&str, f64)) -> Result<(String, bool), String> {
        let cancel = || UP_CANCEL.load(Ordering::Relaxed);
        let h264 = |f: f64| stage("h264", f);
        match self.small_copy(path, rel, stage, &cancel) {
            Ok(Some(p)) => Ok((p.to_string_lossy().to_string(), true)),
            Ok(None) => self.shareable_with(path, &h264, &cancel).map(|p| (p, false)),
            Err(e) => {
                if UP_CANCEL.swap(false, Ordering::Relaxed) {
                    // cancelled by hand: not again on its own (like a cancelled upload)
                    log_line(&format!("cloud: upload of {rel} cancelled while making its Echo Cloud copy"));
                    let mut sk = UP_SKIPPED.lock().unwrap();
                    if !sk.iter().any(|x| x == rel) { sk.push(rel.to_string()); }
                    return Err("Upload cancelled".into());
                }
                Err(e)
            }
        }
    }

    /// Where the Echo Cloud copy of `rel` is kept: (copy, "no gain" marker, the copy's .sig, the clip's size:time).
    fn copy_files(&self, path: &str, rel: &str) -> Result<(PathBuf, PathBuf, PathBuf, String), String> {
        let src = PathBuf::from(path);
        let sm = fs::metadata(&src).map_err(err)?;
        let dir = self.thumbs_dir().join("cloudup");
        fs::create_dir_all(&dir).map_err(err)?;
        // named after the clip's place in the library: two clips with the same name in different folders must never
        // share a copy (the file name in Echo Cloud comes from the clip itself, not from this copy)
        let name = format!("{}.mp4", rel.trim_end_matches(".mp4").trim_end_matches(".MP4").replace(['/', '\\'], "~"));
        Ok((dir.join(&name), dir.join(format!("{name}.orig")), dir.join(format!("{name}.sig")), format!("{}:{}", sm.len(), mtime_secs(&src))))
    }

    /// The smaller Echo Cloud copy of `path` (made now, or made before for the same clip). Ok(None) = upload the clip
    /// itself: under 3 MB, it doesn't save 15 %, or no encoder could make it. Err only when `cancel` said stop
    /// (or the clip can't be read). Doesn't touch the upload box's Cancel state (the background swap uses it too).
    pub(super) fn small_copy(&self, path: &str, rel: &str, stage: &dyn Fn(&str, f64), cancel: &dyn Fn() -> bool) -> Result<Option<PathBuf>, String> {
        let src = PathBuf::from(path);
        let sm = fs::metadata(&src).map_err(err)?;
        let (out, no_gain, copy_sig, sig) = self.copy_files(path, rel)?;
        let dir = out.parent().map(Path::to_path_buf).unwrap_or_default();
        let name = out.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        // copies left by uploads that were cancelled and never tried again
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            if e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).map_or(false, |d| d > Duration::from_secs(7 * 86400)) {
                let _ = fs::remove_file(e.path());
            }
        }
        // made before (an upload that failed or was cancelled) / known not to be worth it
        if fs::read_to_string(&no_gain).map_or(false, |s| s.trim() == sig) {
            return Ok(None);
        }
        if fs::metadata(&out).map_or(false, |m| m.len() > 0) && fs::read_to_string(&copy_sig).map_or(false, |s| s.trim() == sig) {
            return Ok(Some(out));
        }
        let _ = fs::remove_file(&copy_sig);
        // small clips: a few MB either way
        if sm.len() < 3_000_000 {
            return Ok(None);
        }
        let (width, height) = probe_size(&src);
        if height == 0 {
            return Ok(None);
        }
        let scale = height > 1080;
        // the width that keeps the shape at 1080 lines (even) - the GPU scalers want plain numbers
        let w1080 = ((width as f64 * 1080.0 / height as f64 / 2.0).round() as u32 * 2).max(2);
        let tmp = dir.join(format!("{name}.tmp"));
        let t0 = Instant::now();
        let dur = self.thumb(path).map(|t| t.duration).unwrap_or(0.0).max(1.0);
        stage("smaller", 0.0);
        let how = std::cell::RefCell::new(String::new());
        // `hw` = decode (and scale) on the GPU too: the input options and the GPU scaler for clips over 1080p.
        // None = decode on the CPU (as before), then the encoder.
        let run = |venc: &[&str], vaapi: bool, hw: Option<(&[&str], String)>| -> bool {
            if skip_enc(venc) || cancel() { return false; }
            let mut c = ffmpeg();
            c.args(["-hide_banner", "-loglevel", "error", "-progress", "pipe:1", "-nostats", "-y"]);
            if vaapi {
                let Some(d) = vaapi_device() else { return false };
                c.args(["-vaapi_device", &d]);
            }
            if let Some((pre, _)) = &hw { c.args(*pre); }
            // the mix track only (what players hear; the share page plays it too), copied as it is
            // passthrough: every frame keeps its own time (the recorder's clips have uneven frame times; the default
            // made them even by doubling / dropping frames)
            c.arg("-i").arg(&src).args(["-map", "0:v:0", "-map", "0:a:0?", "-c:a", "copy", "-sn", "-dn", "-fps_mode", "passthrough"]);
            let mut vf: Vec<&str> = Vec::new();
            match &hw {
                Some((_, gpu_scale)) => if scale { vf.push(gpu_scale.as_str()); },
                None => if scale { vf.push("scale=-2:1080:flags=bicubic"); },
            }
            if vaapi { vf.push("format=nv12,hwupload"); }
            if !vf.is_empty() { c.args(["-vf", &vf.join(",")]); }
            c.args(venc).args(["-movflags", "+faststart", "-f", "mp4"]).arg(&tmp).stderr(Stdio::null());
            // "Cancel" in the upload box stops the conversion too; its progress shows in the box (av1-upload-fix)
            let ok = ffmpeg_watch(&mut c, dur, &|f| stage("smaller", f), cancel) && fs::metadata(&tmp).map_or(false, |m| m.len() > 0)
                // a GPU decoder that gave up part way must not cut the clip short
                && (hw.is_none() || video_secs(&tmp) >= video_secs(&src) * 0.97);
            if ok { *how.borrow_mut() = format!("{}, {} decode", venc.get(1).unwrap_or(&""), if hw.is_some() { "GPU" } else { "CPU" }); }
            ok
        };
        // GPU decoding (faster-share-links, 11 Nov 2026, measured on an RX 9070 XT): only for AV1 clips. This ffmpeg
        // decodes AV1 on the CPU with libaom (slow): the GPU halved those encodes. H.264 clips were FASTER with the
        // CPU decoding them (16 threads) - handing every frame from the GPU decoder to the encoder cost more.
        let gpu_dec = is_av1(path);
        // AMF: vpp_amf scales on the GPU - bicubic: its default (bilinear) lost 5 VMAF points on a 1440p clip
        // (d3d11va + scale_d3d11 couldn't be used here: "Could not create the texture")
        let amf_hw: (&[&str], String) = (&AMF_DEC, format!("vpp_amf=w={w1080}:h=1080:scale_type=bicubic"));
        // ⚠ NVENC / QSV GPU decoding: not tried on a real NVIDIA / Intel PC (this PC is AMD only)
        let nv_hw: (&[&str], String) = (&["-hwaccel", "cuda", "-hwaccel_output_format", "cuda", "-extra_hw_frames", "24"], format!("scale_cuda={w1080}:1080:interp_algo=bicubic"));
        let qsv_hw: (&[&str], String) = (&["-hwaccel", "qsv", "-hwaccel_output_format", "qsv", "-extra_hw_frames", "24"], format!("scale_qsv=w={w1080}:h=1080:mode=hq"));
        let amf = ["-c:v", "h264_amf", "-quality", "speed", "-rc", "cqp", "-qp_i", QP_AMF[0], "-qp_p", QP_AMF[1], "-qp_b", QP_AMF[2]];
        // ⚠ p4 (was p5): a bit faster, about the same size at the same -cq - not measured (no NVIDIA here)
        let nv = ["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", Q_NVENC, "-b:v", "0", "-maxrate", MAX_RATE, "-bufsize", BUF];
        let nv_cpu = [&nv[..], &["-pix_fmt", "yuv420p"]].concat();
        let qsv = ["-c:v", "h264_qsv", "-global_quality", Q_QSV, "-maxrate", MAX_RATE, "-bufsize", BUF];
        // each GPU path falls back to its CPU-decoding one (old drivers, odd formats, a GPU that can't decode it);
        // the recorder may be using the GPU encoder at the same moment: AMF then sometimes fails at once - so AMF
        // gets two more tries
        let ok = (gpu_dec && run(&amf, false, Some(amf_hw))) || run(&amf, false, None) || run(&amf, false, None)
            || (gpu_dec && run(&nv, false, Some(nv_hw))) || run(&nv_cpu, false, None)
            || (gpu_dec && run(&qsv, false, Some(qsv_hw))) || run(&qsv, false, None)
            || (cfg!(target_os = "linux") && run(&["-c:v", "h264_vaapi", "-rc_mode", "CQP", "-qp", Q_VAAPI], true, None))
            || run(&["-c:v", "libx264", "-preset", "veryfast", "-crf", Q_X264, "-maxrate", MAX_RATE, "-bufsize", BUF, "-pix_fmt", "yuv420p"], false, None);
        if cancel() {
            let _ = fs::remove_file(&tmp);
            return Err("Upload cancelled".into());
        }
        if !ok {
            let _ = fs::remove_file(&tmp);
            log_line(&format!("echo cloud: couldn't make a smaller copy of {rel} - uploading the clip itself"));
            return Ok(None);
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
            return Ok(None);
        }
        let _ = fs::remove_file(&out);
        fs::rename(&tmp, &out).map_err(err)?;
        let _ = fs::write(&copy_sig, &sig);
        self.note_speed(None, Some((dur, t0.elapsed().as_secs_f64())));
        log_line(&format!("echo cloud: copy of {rel} for Echo Cloud: {:.1} MB -> {:.1} MB ({:.0} % smaller{}) in {:.0} s ({})",
            base as f64 / 1e6, small as f64 / 1e6, 100.0 - small as f64 * 100.0 / base.max(1) as f64,
            if scale { ", 1080p" } else { "" }, t0.elapsed().as_secs_f32(), how.borrow()));
        Ok(Some(out))
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
        // faster-share-links: the server knows "share first, shrink after" (an older one doesn't say so)
        SHARE_FIRST_OK.store(j["share_first"].as_bool().unwrap_or(false), Ordering::Relaxed);
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

    /// av1-upload-fix (10 Nov 2026): does a share link for this clip need an H.264 share copy - would it
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

    /// av1-upload-fix (10 Nov 2026): a share link for an AV1 clip on the user's own clouds (no Echo Cloud).
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

// ----- share first, shrink after (faster-share-links, 11 Nov 2026: long clips only, the user's choice) -----
// "Copy share link" on a long clip that's only on this PC: the clip goes up AS IT IS first (to a hidden place in Echo
// Cloud - the server's swaps table), so the link works as soon as that upload is done. Echo then makes the smaller
// copy in the background (run_swaps, started by the app's background loop), uploads it to the clip's real place and
// tells the server (/upload/swap): from then on the link plays the small copy and only that counts against the plan.
// Short clips keep the old way (copy first, then upload). Needs a server that says share_first in /upload/check
// (an older one: the old way). Cancel during the upload works as before; the background part has no box.

/// Clips at least this long (seconds) are shared first and made smaller afterwards. The copy's encode time follows
/// the clip's length (frames), not its size: 2 min = about 15-25 s of encoding on an RX 9070 XT (step 1 table).
pub(super) const SHARE_FIRST_SECS: f64 = 120.0;
/// the server's last /upload/check said it can do share first
static SHARE_FIRST_OK: AtomicBool = AtomicBool::new(false);
/// when run_swaps has something to do (unix s; 0 = look at swaps.json, u64::MAX = nothing waiting)
static SWAPS_DUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SWAPS_RUNNING: AtomicBool = AtomicBool::new(false);

/// One clip whose original is in Echo Cloud for its link while the smaller copy is still to come.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub(super) struct SwapJob {
    rel: String,
    /// the clip's place in Echo Cloud ("Echo/2026/11/11/<folder>/<name>.mp4")
    key: String,
    /// the clip on this PC and its size:time when it went up
    path: String,
    sig: String,
    hash: Option<String>,
    share_only: bool,
    orig_size: u64,
    #[serde(default)]
    tries: u32,
    #[serde(default)]
    next: u64,
    /// when the original went up (unix s; 0 = a job from before this field)
    #[serde(default)]
    created: u64,
}

/// Measured on this PC (running averages): upload speed to Echo Cloud in bytes/s, and encode seconds per second
/// of clip for the smaller copy. 0 = not measured yet.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Copy, Debug)]
pub(super) struct Speeds { up: f64, enc: f64 }

impl Recorder {
    fn speeds_file(&self) -> PathBuf { self.thumbs_dir().join("cloudup").join("speeds.json") }
    pub(super) fn speeds(&self) -> Speeds {
        fs::read_to_string(self.speeds_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    /// an Echo Cloud upload of `up` = (bytes, seconds) / a smaller copy of `enc` = (clip seconds, encode seconds)
    pub(super) fn note_speed(&self, up: Option<(u64, f64)>, enc: Option<(f64, f64)>) {
        let mut s = self.speeds();
        let avg = |old: f64, new: f64| if old > 0.0 { old * 0.6 + new * 0.4 } else { new };
        // small files say little about the line (start-up time)
        if let Some((b, t)) = up.filter(|(b, t)| *b >= 5_000_000 && *t >= 0.5) { s.up = avg(s.up, b as f64 / t); }
        if let Some((d, t)) = enc.filter(|(d, t)| *d >= 10.0 && *t > 0.0) { s.enc = avg(s.enc, t / d); }
        let _ = fs::create_dir_all(self.thumbs_dir().join("cloudup"));
        let _ = fs::write(self.speeds_file(), serde_json::to_string(&s).unwrap_or_default());
    }

    fn swaps_file(&self) -> PathBuf { self.thumbs_dir().join("cloudup").join("swaps.json") }
    fn swaps(&self) -> Vec<SwapJob> {
        fs::read_to_string(self.swaps_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn save_swaps(&self, v: &[SwapJob]) {
        let _ = fs::create_dir_all(self.thumbs_dir().join("cloudup"));
        let _ = write_patient(&self.swaps_file(), &serde_json::to_string(v).unwrap_or_default());
    }
    /// `rel`'s original is up for its link and its smaller copy is still to come (its upload is under way)
    pub(super) fn swap_pending(&self, rel: &str) -> bool {
        SWAPS_DUE.load(Ordering::Relaxed) != u64::MAX && self.swaps().iter().any(|j| j.rel == rel)
    }

    /// upload_inner, for a share link: the clip as it is, first (see above). None = not this way (short clip, a copy
    /// made before, older server, ...): the caller goes on the old way. Some(Ok(None)) = up, the link can be made.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn share_first(&self, path: &str, rel: &str, cp: &str, ec_key: &str, hash: Option<&str>, share_only: bool, made: u64) -> Option<Result<Option<String>, String>> {
        if !SHARE_FIRST_OK.load(Ordering::Relaxed) || hash.is_none() || is_av1(path) { return None; }
        let sm = fs::metadata(path).ok()?;
        let dur = self.thumb(path).map(|t| t.duration).unwrap_or(0.0);
        if sm.len() < 3_000_000 || dur < SHARE_FIRST_SECS { return None; }
        // the original is about 3x the smaller copy: on a slow line sending it takes longer than making the copy and
        // sending that (with this PC's measured speeds; not measured yet = share first)
        let sp = self.speeds();
        if sp.up > 0.0 {
            let first = sm.len() as f64 / sp.up;
            let old = dur * if sp.enc > 0.0 { sp.enc } else { 0.15 } + sm.len() as f64 * 0.35 / sp.up;
            if first > old {
                log_line(&format!("echo cloud: {rel}: smaller copy first - quicker here ({:.1} MB/s upload: {first:.0} s for the clip vs about {old:.0} s)", sp.up / 1e6));
                return None;
            }
        }
        // a copy made before / known not to be worth it: the old way is just as quick (nothing to encode)
        let (out, no_gain, copy_sig, sig) = self.copy_files(path, rel).ok()?;
        let has = |f: &Path| fs::read_to_string(f).map_or(false, |s| s.trim() == sig);
        if has(&no_gain) || (has(&copy_sig) && out.exists()) { return None; }
        // a clip that's in Echo Cloud already (this is a re-upload): the old way
        if self.cloud_state().files.iter().any(|f| f.rel == rel) { return None; }
        let cfg = self.settings();
        let t0 = Instant::now();
        let mut req = serde_json::json!({ "path": ec_key, "size": sm.len(), "hash": hash, "allow_dup": true, "share_first": true });
        if share_only { req["share_only"] = serde_json::json!(true); }
        let j = match ec_call(&ec_api(&cfg), "POST", "/upload", &cfg.ec_token, Some(req)) {
            Ok(j) => j,
            // e.g. full: the smaller copy may still fit - the old way decides
            Err(e) => { log_line(&format!("echo cloud: share first for {rel} not possible ({e}) - making the smaller copy first")); return None; }
        };
        if j["swap"].as_str().is_none() { return None; }
        let url = j["url"].as_str().unwrap_or("").to_string();
        { let mut p = UP_PROGRESS.lock().unwrap(); if p.rel == rel { p.total = sm.len(); p.stage = "send".into(); p.stage_f = 0.0; } }
        let rel2 = rel.to_string();
        let tp = Instant::now();
        let r = crate::net::put_file(&url, Path::new(path), move |b, t| {
            let mut p = UP_PROGRESS.lock().unwrap();
            if p.rel == rel2 { p.got = b; p.total = t; }
        }, || UP_CANCEL.load(Ordering::Relaxed));
        if r.is_ok() { self.note_speed(Some((sm.len(), tp.elapsed().as_secs_f64())), None); }
        if let Err(e) = r {
            // (the server forgets the half-done upload by itself: its daily job)
            if UP_CANCEL.swap(false, Ordering::Relaxed) {
                log_line(&format!("cloud: upload of {rel} to Echo Cloud cancelled"));
                let mut sk = UP_SKIPPED.lock().unwrap();
                if !sk.iter().any(|x| x == rel) { sk.push(rel.to_string()); }
                return Some(Err("Upload cancelled".into()));
            }
            log_line(&format!("echo cloud: upload of {rel} failed: {e}"));
            return Some(Err(format!("Upload to Echo Cloud failed: {e}")));
        }
        ec_add_used(sm.len());
        let kept_share_only = j["share_only"].as_bool().unwrap_or(false);
        // the cloud list knows it now (the server's listing shows it once the smaller copy is in its place)
        let mut st = self.cloud_state();
        st.keep_remote.remove(rel);
        st.no_upload.remove(rel);
        st.dups.remove(rel);
        if kept_share_only { st.share_only.insert(rel.to_string()); } else { st.share_only.remove(rel); }
        st.smaller.remove(rel);
        st.h264.remove(rel);
        st.files.retain(|f| f.rel != rel);
        let p = Path::new(rel);
        st.files.push(CloudFile {
            rel: rel.to_string(), path: cp.to_string(),
            name: p.file_stem().unwrap_or_default().to_string_lossy().into(),
            folder: p.parent().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(),
            size: sm.len(), modified: made, local: true,
        });
        self.save_cloud_state(&st);
        let mut v = self.swaps();
        v.retain(|x| x.rel != rel);
        v.push(SwapJob { rel: rel.to_string(), key: ec_key.to_string(), path: path.to_string(), sig, hash: hash.map(String::from),
            share_only: kept_share_only, orig_size: sm.len(), tries: 0, next: 0, created: now_unix() });
        self.save_swaps(&v);
        SWAPS_DUE.store(0, Ordering::Relaxed);
        log_line(&format!("cloud: uploaded {rel} to Echo Cloud as it is in {:.0}s ({:.1} MB) - its link works now, the smaller copy follows (share first)",
            t0.elapsed().as_secs_f32(), sm.len() as f64 / 1e6));
        Some(Ok(None))
    }

    /// The app's background loop asks this every few seconds: run run_swaps() on a thread of its own when true.
    pub fn swaps_waiting(&self) -> bool {
        !SWAPS_RUNNING.load(Ordering::Relaxed) && SWAPS_DUE.load(Ordering::Relaxed) <= now_unix()
    }

    /// Make the smaller copies of share-first uploads and put them in place (one after the other). Also picks up
    /// ones left when Echo was closed. A job that keeps failing is tried again later (5, 10, ... min) and dropped
    /// after 6 tries - the server then finishes it by itself after a day (keeps the original).
    pub fn run_swaps(&self) {
        if SWAPS_RUNNING.swap(true, Ordering::Relaxed) { return; }
        struct Running;
        impl Drop for Running { fn drop(&mut self) { SWAPS_RUNNING.store(false, Ordering::Relaxed); } }
        let _r = Running;
        loop {
            let t = now_unix();
            let Some(job) = self.swaps().into_iter().find(|j| j.next <= t) else { break };
            let r = self.finish_swap(&job);
            let mut v = self.swaps();
            match r {
                Ok(()) => v.retain(|x| !(x.rel == job.rel && x.key == job.key)),
                Err(e) => {
                    log_line(&format!("echo cloud: smaller copy of {} not in place yet: {e}", job.rel));
                    // offline, asleep, Echo Cloud busy...: again in 5, 10, ... min (at most hourly). After 3 days
                    // the server has kept the original by itself (counted in full) and the daily sweep takes over.
                    if let Some(x) = v.iter_mut().find(|x| x.rel == job.rel && x.key == job.key) {
                        x.tries += 1;
                        x.next = t + (300 * x.tries as u64).min(3600);
                        if x.created == 0 { x.created = t; }
                    }
                    v.retain(|x| x.created == 0 || x.created + 3 * 86400 > t);
                }
            }
            self.save_swaps(&v);
        }
        let next = self.swaps().iter().map(|j| j.next).min().unwrap_or(u64::MAX);
        SWAPS_DUE.store(next, Ordering::Relaxed);
    }

    /// One share-first clip: its smaller copy goes to the clip's place in Echo Cloud, the server swaps the link over.
    /// No smaller copy (clip gone / changed / it wouldn't save enough): the server keeps the original as the clip.
    fn finish_swap(&self, job: &SwapJob) -> Result<(), String> {
        let cfg = self.settings();
        // signed out of Echo Cloud: nothing to do here (the server finishes it)
        if cfg.ec_token.is_empty() { return Ok(()); }
        let api = ec_api(&cfg);
        let t0 = Instant::now();
        // QA E4 (11 Nov 2026): first ask whether this swap still waits. While it does, the clip's place is empty and
        // this call changes nothing ("upload the smaller copy first"). "done" = finished meanwhile (the server kept the
        // original after a day, or a copy got there before Echo closed); "swap: false" = no swap any more (deleted,
        // renamed, cleaned up). Then nothing is uploaded here - the daily sweep makes a kept original smaller.
        match ec_call(&api, "POST", "/upload/swap", &cfg.ec_token, Some(serde_json::json!({ "path": job.key }))) {
            Err(e) if e.contains("smaller copy first") => {}
            Ok(j) => {
                log_line(&format!("echo cloud: {} - no smaller copy to swap in any more ({}) - left to the daily sweep", job.rel,
                    if j["done"].as_bool() == Some(true) { "finished meanwhile" } else { "deleted, moved or cleaned up" }));
                return Ok(());
            }
            Err(e) if e.contains("not found") => return Ok(()),   // a server without swaps
            Err(e) => return Err(e),                               // offline...: again later
        }
        let same = fs::metadata(&job.path).ok().map(|m| format!("{}:{}", m.len(), mtime_secs(Path::new(&job.path)))) == Some(job.sig.clone());
        let small = if same { self.small_copy(&job.path, &job.rel, &|_, _| {}, &|| false)? } else { None };
        let Some(small) = small else {
            // keep = on purpose (no gain / tiny); else the original stays for now and the daily sweep tries again
            let keep = same && self.orig_fmt(&job.path, &job.rel) == "keep";
            ec_call(&api, "POST", "/upload/swap", &cfg.ec_token, Some(serde_json::json!({ "path": job.key, "keep_original": true, "keep": keep })))?;
            log_line(&format!("echo cloud: {} stays in Echo Cloud as it is (no smaller copy{})", job.rel, if same { "" } else { " - the clip changed or is gone" }));
            return Ok(());
        };
        let size = fs::metadata(&small).map_err(err)?.len();
        let mut req = serde_json::json!({ "path": job.key, "size": size, "allow_dup": true, "fmt": "small" });
        if let Some(h) = &job.hash { req["hash"] = serde_json::json!(h); }
        if job.share_only { req["share_only"] = serde_json::json!(true); }
        let j = ec_call(&api, "POST", "/upload", &cfg.ec_token, Some(req))?;
        if j["swap"].is_string() { return Err("the server didn't take the smaller copy".into()); }
        crate::net::put_file(j["url"].as_str().unwrap_or(""), &small, |_, _| {}, || false)?;
        let w = ec_call(&api, "POST", "/upload/swap", &cfg.ec_token, Some(serde_json::json!({ "path": job.key })))?;
        if w["swap"].as_bool() == Some(false) {
            // renamed / deleted in the few seconds since the check above: the copy stays where it went - never delete
            // here (QA E4: that removed a clip and its link when the server had finished the swap by itself)
            log_line(&format!("echo cloud: {} was moved or deleted while its smaller copy went up - left as it is", job.rel));
        } else {
            let mut st = self.cloud_state();
            st.smaller.insert(job.rel.clone(), [size, job.orig_size]);
            if let Some(f) = st.files.iter_mut().find(|f| f.rel == job.rel) { f.size = size; }
            self.save_cloud_state(&st);
            if self.cloud_serve.lock().unwrap().is_some() { SERVE_STALE.store(true, Ordering::Relaxed); }
            log_line(&format!("echo cloud: smaller copy of {} in place ({:.1} -> {:.1} MB, {:.0} s) - its link plays it now",
                job.rel, job.orig_size as f64 / 1e6, size as f64 / 1e6, t0.elapsed().as_secs_f32()));
        }
        let _ = fs::remove_file(&small);
        let _ = fs::remove_file(format!("{}.sig", small.to_string_lossy()));
        Ok(())
    }

    /// The clip itself going up to Echo Cloud: "keep" when that's on purpose (a smaller copy wouldn't save 15 %, or
    /// it's under 3 MB), else "orig" (no copy could be made now: the daily sweep tries again).
    pub(super) fn orig_fmt(&self, path: &str, rel: &str) -> &'static str {
        let Ok((_, no_gain, _, sig)) = self.copy_files(path, rel) else { return "orig" };
        let tiny = fs::metadata(path).map_or(false, |m| m.len() < 3_000_000);
        if tiny || fs::read_to_string(&no_gain).map_or(false, |s| s.trim() == sig) { "keep" } else { "orig" }
    }

    fn heal_file(&self) -> PathBuf { self.thumbs_dir().join("cloudup").join("sweep.json") }

    /// The app's idle loop asks this (every 10 min while the PC is idle and no game runs): time for the daily sweep?
    pub fn heal_due(&self) -> bool {
        let cfg = self.settings();
        if cfg.ec_token.is_empty() || !cfg.cloud_accounts.iter().any(|a| a.provider == "echocloud") { return false; }
        let last = fs::read_to_string(self.heal_file()).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|j| j["last"].as_u64()).unwrap_or(0);
        now_unix() >= last + 86400
    }

    /// The daily sweep (faster-share-links, 11 Nov 2026): clips that are in Echo Cloud AS THEY ARE (an original kept
    /// after an interrupted share-first upload, a copy that couldn't be made then, an upload from an older Echo) and
    /// whose clip is still on this PC get their smaller copy now, put in the same place (same links).
    /// - Which ones: the cloud file is exactly the clip's size, Echo didn't upload a copy of it (cloud state), and the
    ///   server doesn't call it "small" / "keep" (/objects/fmt). With a fingerprint on the server, it must match the clip's.
    /// - Clips no longer on this PC are left as they are (counted in full against the plan) - no big downloads just to
    ///   shrink them; the log lists how many.
    /// - Not while a share link plays it (read in the last hour), one at a time, stops as soon as `calm` says the PC is
    ///   busy again (the encode is cancelled; the next sweep starts over).
    /// - A server without /objects/fmt (older): nothing happens.
    pub fn heal_originals(&self, calm: &dyn Fn() -> bool) {
        // marked first: a crash in here must not make it run again at once
        let _ = fs::create_dir_all(self.thumbs_dir().join("cloudup"));
        let _ = fs::write(self.heal_file(), serde_json::json!({ "last": now_unix() }).to_string());
        let cfg = self.settings();
        let api = ec_api(&cfg);
        let root = self.clips_dir();
        let st = self.cloud_state();
        // (rel, Echo Cloud path, clip on this PC) - size the same as the cloud file = the cloud has the clip itself
        let mut local: Vec<(String, String, PathBuf)> = Vec::new();
        let mut away: Vec<String> = Vec::new();
        for f in &st.files {
            if f.path.is_empty() || st.smaller.contains_key(&f.rel) || st.h264.contains(&f.rel) || self.swap_pending(&f.rel) { continue; }
            let p = root.join(&f.rel);
            match fs::metadata(&p) {
                Ok(m) if m.len() == f.size => local.push((f.rel.clone(), format!("Echo/{}", f.path.trim_start_matches('/')), p)),
                Ok(_) => {}
                Err(_) => away.push(format!("Echo/{}", f.path.trim_start_matches('/'))),
            }
        }
        let ask = |paths: Vec<&String>| -> Option<serde_json::Map<String, serde_json::Value>> {
            let mut all = serde_json::Map::new();
            for part in paths.chunks(200) {
                let j = ec_call(&api, "POST", "/objects/fmt", &cfg.ec_token, Some(serde_json::json!({ "paths": part }))).ok()?;
                if let Some(m) = j["files"].as_object() { all.extend(m.clone()); }
            }
            Some(all)
        };
        let Some(info) = ask(local.iter().map(|x| &x.1).collect()) else { return };
        let t = now_unix();
        let (mut done, mut kept, mut skipped) = (0, 0, 0);
        // QA 11 Nov 2026: the server leaves clips without an objects row (older uploads) out of its answer, and
        // indexing a Map with a missing key panics - that killed the idle thread (and Shrink old clips) every day
        let null = serde_json::Value::Null;
        for (rel, key, p) in &local {
            let i = info.get(key.as_str()).unwrap_or(&null);
            if matches!(i["fmt"].as_str(), Some("small") | Some("keep")) { continue; }
            if i["last_read"].as_u64().unwrap_or(0) + 3600 > t { skipped += 1; continue; }
            if !calm() { break; }
            let path = p.to_string_lossy().to_string();
            let hash = self.clip_hash(p, rel);
            // the cloud knows this clip's fingerprint: it must be this very clip
            if let Some(h) = i["hash"].as_str() { if hash.as_deref() != Some(h) { skipped += 1; continue; } }
            let small = match self.small_copy(&path, rel, &|_, _| {}, &|| !calm()) {
                Ok(s) => s,
                Err(_) => break,   // the PC got busy: next time
            };
            let Some(small) = small else {
                if self.orig_fmt(&path, rel) == "keep" {
                    let _ = ec_call(&api, "POST", "/objects/mark", &cfg.ec_token, Some(serde_json::json!({ "path": key, "fmt": "keep" })));
                    kept += 1;
                }
                continue;
            };
            let size = fs::metadata(&small).map(|m| m.len()).unwrap_or(0);
            // (an AV1 clip's copy is compared with twice the clip in small_copy - here the cloud holds the AV1 file itself)
            let cloud_size = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            if size as f64 > cloud_size as f64 * (1.0 - MIN_SAVING) {
                let _ = ec_call(&api, "POST", "/objects/mark", &cfg.ec_token, Some(serde_json::json!({ "path": key, "fmt": "keep" })));
                kept += 1;
                continue;
            }
            let share_only = self.cloud_state().share_only.contains(rel);
            let mut req = serde_json::json!({ "path": key, "size": size, "allow_dup": true, "fmt": "small" });
            if let Some(h) = &hash { req["hash"] = serde_json::json!(h); }
            // a share-only upload stays share-only (it still goes when its link does)
            if share_only { req["share_only"] = serde_json::json!(true); }
            let up = ec_call(&api, "POST", "/upload", &cfg.ec_token, Some(req))
                .and_then(|j| crate::net::put_file(j["url"].as_str().unwrap_or(""), &small, |_, _| {}, || false));
            match up {
                Ok(()) => {
                    let mut st = self.cloud_state();
                    let orig = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                    st.smaller.insert(rel.clone(), [size, orig]);
                    if let Some(f) = st.files.iter_mut().find(|f| f.rel == *rel) { f.size = size; }
                    self.save_cloud_state(&st);
                    if self.cloud_serve.lock().unwrap().is_some() { SERVE_STALE.store(true, Ordering::Relaxed); }
                    log_line(&format!("echo cloud: sweep - {rel} in Echo Cloud made smaller ({:.1} -> {:.1} MB)", orig as f64 / 1e6, size as f64 / 1e6));
                    done += 1;
                }
                Err(e) => log_line(&format!("echo cloud: sweep - {rel}: {e}")),
            }
            let _ = fs::remove_file(&small);
            let _ = fs::remove_file(format!("{}.sig", small.to_string_lossy()));
        }
        // originals of clips that aren't on this PC any more: only counted (another PC may still have them)
        let orig_away = if away.is_empty() { 0 } else {
            ask(away.iter().collect()).map_or(0, |m| m.values().filter(|v| v["fmt"].as_str() == Some("orig")).count())
        };
        log_line(&format!("echo cloud: sweep - {done} made smaller, {kept} stay as they are (no gain), {skipped} skipped (playing / other file){}",
            if orig_away > 0 { format!(", {orig_away} originals whose clip isn't on this PC left as they are") } else { String::new() }));
    }
}

#[cfg(test)]
mod cloud_copy_tests {
    use super::*;

    /// faster-share-links (11 Nov 2026): makes the real Echo Cloud copy of every .mp4 in ECHO_CC_DIR (scratch copies,
    /// never the real clips - the copies land in ECHO_CC_DIR/.thumbs/cloudup) and prints size + time + how.
    /// Set TEMP to a scratch folder too: log lines go to TEMP/clipper_ffmpeg.log.
    /// cargo test -p echo-core --release --lib cloud_copy_real_clips -- --ignored --nocapture
    #[test]
    #[ignore]
    fn cloud_copy_real_clips() {
        let Ok(dir) = std::env::var("ECHO_CC_DIR") else { return };
        let mut cfg = crate::settings::Settings::default();
        cfg.clips_dir = dir.clone();
        let rec = Recorder::new(cfg);
        let mut clips: Vec<PathBuf> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "mp4")).collect();
        clips.sort();
        println!("| clip | source | copy | time | log |");
        println!("|---|---|---|---|---|");
        for src in &clips {
            let rel = src.file_name().unwrap().to_string_lossy().to_string();
            let t = Instant::now();
            let r = rec.cloud_copy(&src.to_string_lossy(), &rel, &|_, _| {});
            let secs = t.elapsed().as_secs_f32();
            let last = fs::read_to_string(log_path()).unwrap_or_default().lines().rev().find(|l| l.contains(&rel)).unwrap_or("").to_string();
            match r {
                Ok((p, tmp)) => println!("| {rel} | {:.1} MB | {:.1} MB{} | {secs:.1} s | {last} |", fs::metadata(src).unwrap().len() as f64 / 1e6,
                    fs::metadata(&p).map(|m| m.len()).unwrap_or(0) as f64 / 1e6, if tmp { "" } else { " (the clip itself)" }),
                Err(e) => println!("| {rel} | - | ERROR {e} | {secs:.1} s | {last} |"),
            }
        }
    }

    /// faster-share-links: the AV1 share copy (share_copy) and H.264 copy (make_h264) of every AV1 .mp4 in ECHO_CC_DIR.
    /// cargo test -p echo-core --release --lib share_copy_real_clips -- --ignored --nocapture
    #[test]
    #[ignore]
    fn share_copy_real_clips() {
        let Ok(dir) = std::env::var("ECHO_CC_DIR") else { return };
        let mut cfg = crate::settings::Settings::default();
        cfg.clips_dir = dir.clone();
        let rec = Recorder::new(cfg);
        let out = Path::new(&dir).join("h264test");
        let _ = fs::create_dir_all(&out);
        let mut clips: Vec<PathBuf> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "mp4") && is_av1(&p.to_string_lossy())).collect();
        clips.sort();
        for src in &clips {
            let t = Instant::now();
            let r = rec.share_copy(src, &|_| {});
            println!("share_copy {}: {:?} {:.1} MB in {:.1} s", src.display(), r.as_ref().err(), r.as_ref().ok().and_then(|p| fs::metadata(p).ok()).map_or(0, |m| m.len()) as f64 / 1e6, t.elapsed().as_secs_f32());
            let o = out.join(src.file_name().unwrap());
            let t = Instant::now();
            let r = rec.make_h264(src, &o, 0.0, &|_| {}, &|| false);
            println!("make_h264 {}: {:?} {:.1} MB in {:.1} s", src.display(), r.err(), fs::metadata(&o).map_or(0, |m| m.len()) as f64 / 1e6, t.elapsed().as_secs_f32());
        }
    }

    /// faster-share-links: "share first, shrink after" end to end against the local test server (scratchpad
    /// harness.mjs: in-memory R2 + SQLite, never the real Echo Cloud). Needs ECHO_CLOUD_API=http://127.0.0.1:<port>,
    /// ECHO_SF_DIR = clip copies (long.mp4 = an H.264 clip over 2 min, short.mp4 = one under 2 min), APPDATA + TEMP
    /// pointed at scratch folders. ECHO_SF_OLD=1: the server is the old one (no share first) - the old way must run.
    /// cargo test -p echo-core --release --lib share_first_end_to_end -- --ignored --nocapture
    #[test]
    #[ignore]
    fn share_first_end_to_end() {
        let (Ok(dir), Ok(api)) = (std::env::var("ECHO_SF_DIR"), std::env::var("ECHO_CLOUD_API")) else { return };
        let old_server = std::env::var("ECHO_SF_OLD").is_ok();
        let mut cfg = crate::settings::Settings::default();
        cfg.clips_dir = dir.clone();
        cfg.cloud = "echocloud".into();
        cfg.cloud_accounts = vec![crate::settings::CloudAccount { id: 1, provider: "echocloud".into(), label: "Echo Cloud".into(), root: String::new() }];
        cfg.ec_token = "tok_harness_test".into();
        cfg.ec_api = api.clone();
        cfg.link_days = 7;
        let rec = Recorder::new(cfg);
        let state = || crate::net::get_json(&format!("{api}/__state")).unwrap();
        let r2_size = |end: &str| state()["r2"].as_array().unwrap().iter().find(|o| o["key"].as_str().unwrap().ends_with(end)).and_then(|o| o["size"].as_u64());
        let served = |url: &str| { let p = Path::new(&dir).join("served.tmp"); crate::net::download(&format!("{url}.mp4"), &p).unwrap(); let n = fs::metadata(&p).unwrap().len(); let _ = fs::remove_file(&p); n };
        let long = Path::new(&dir).join("long.mp4").to_string_lossy().to_string();
        let short = Path::new(&dir).join("short.mp4").to_string_lossy().to_string();
        let long_size = fs::metadata(&long).unwrap().len();

        // long clip: the link comes back after the plain upload, the smaller copy follows
        let t = Instant::now();
        let url = rec.upload_and_link(&long, Some(7), &|_| {}).expect("long clip link");
        let secs = t.elapsed().as_secs_f32();
        let s = state();
        println!("long clip: link in {secs:.1} s: {url}\n  swaps: {}", s["swaps"]);
        if old_server {
            assert!(s["swaps"].as_array().map_or(true, |a| a.is_empty()), "old server: no swap");
            assert!(served(&url) < long_size, "old server: the link plays the smaller copy");
            println!("PASS old server: old way (smaller copy first, {:.1} MB)", served(&url) as f64 / 1e6);
        } else {
            assert_eq!(s["swaps"].as_array().unwrap().len(), 1, "a pending swap");
            assert_eq!(served(&url), long_size, "the link plays the original right away");
            assert!(rec.swaps_waiting(), "background job waiting");
            assert!(rec.swap_pending("long.mp4"));
            // a second "Copy share link" while the copy is being made: the same link, nothing uploaded again
            let url2 = rec.upload_and_link(&long, Some(7), &|_| {}).expect("again");
            assert_eq!(url, url2, "same link while pending");
            let t = Instant::now();
            rec.run_swaps();
            let small = served(&url);
            println!("  background copy + swap in {:.1} s: link now plays {:.1} MB (original {:.1} MB)", t.elapsed().as_secs_f32(), small as f64 / 1e6, long_size as f64 / 1e6);
            assert!(small < long_size, "the link plays the smaller copy after the swap");
            let s = state();
            assert!(s["swaps"][0]["done"].as_u64().unwrap_or(0) > 0, "swap done on the server");
            assert_eq!(r2_size("long.mp4"), Some(small), "R2 holds the small copy at the clip's key");
            assert!(!rec.swap_pending("long.mp4"), "nothing pending");
            assert!(!rec.swaps_waiting() || rec.swaps().is_empty(), "job list empty");
            let st = rec.cloud_state();
            assert_eq!(st.smaller.get("long.mp4").map(|v| v[0]), Some(small), "cloud state knows the smaller copy");
            println!("PASS share first: link in {secs:.1} s, smaller copy swapped in");
            // a slow line (1 MB/s): sending the whole clip takes longer than copy + send -> the old way
            let long2 = Path::new(&dir).join("long2.mp4");
            if long2.exists() {
                let _ = fs::write(rec.speeds_file(), r#"{"up":1000000.0,"enc":0.12}"#);
                rec.upload_and_link(&long2.to_string_lossy(), Some(7), &|_| {}).expect("long2 link");
                let n = state()["swaps"].as_array().map_or(0, |a| a.iter().filter(|x| x["key"].as_str().unwrap_or("").ends_with("long2.mp4")).count());
                assert_eq!(n, 0, "slow line: no share first");
                println!("PASS slow line: smaller copy first");
            }
        }

        // short clip: the old way (smaller copy first, no swap)
        let t = Instant::now();
        let url = rec.upload_and_link(&short, Some(7), &|_| {}).expect("short clip link");
        let n = state()["swaps"].as_array().map_or(0, |a| a.iter().filter(|x| x["key"].as_str().unwrap_or("").ends_with("short.mp4")).count());
        assert_eq!(n, 0, "short clip: no swap");
        assert!(served(&url) < fs::metadata(&short).unwrap().len(), "short clip: smaller copy");
        println!("PASS short clip: old way, link in {:.1} s", t.elapsed().as_secs_f32());
    }

    /// faster-share-links: resume after Echo was closed / offline, and the daily sweep - end to end against the local
    /// test server (harness.mjs). Needs ECHO_CLOUD_API, ECHO_SF_DIR (long.mp4 = H.264 over 2 min, legacy.mp4 = another
    /// H.264 clip over 3 MB), APPDATA + TEMP pointed at scratch folders.
    /// cargo test -p echo-core --release --lib resume_and_sweep_end_to_end -- --ignored --nocapture
    #[test]
    #[ignore]
    fn resume_and_sweep_end_to_end() {
        let (Ok(dir), Ok(api)) = (std::env::var("ECHO_SF_DIR"), std::env::var("ECHO_CLOUD_API")) else { return };
        let mk = |api: &str| {
            let mut cfg = crate::settings::Settings::default();
            cfg.clips_dir = dir.clone();
            cfg.cloud = "echocloud".into();
            cfg.cloud_accounts = vec![crate::settings::CloudAccount { id: 1, provider: "echocloud".into(), label: "Echo Cloud".into(), root: String::new() }];
            cfg.ec_token = "tok_harness_test".into();
            cfg.ec_api = api.to_string();
            cfg.link_days = 7;
            Recorder::new(cfg)
        };
        let get = |p: &str| crate::net::get_json(&format!("{api}{p}")).unwrap();
        let state = || get("/__state");
        let r2_size = |end: &str| state()["r2"].as_array().unwrap().iter().find(|o| o["key"].as_str().unwrap().ends_with(end)).and_then(|o| o["size"].as_u64());
        let long = Path::new(&dir).join("long.mp4");
        let long_size = fs::metadata(&long).unwrap().len();

        // 1. share first, then the background job fails (Echo Cloud unreachable) ...
        let rec = mk(&api);
        let url = rec.upload_and_link(&long.to_string_lossy(), Some(0), &|_| {}).expect("link");
        assert_eq!(rec.swaps().len(), 1, "a pending shrink");
        std::env::set_var("ECHO_CLOUD_API", "http://127.0.0.1:9");   // nothing listens there
        rec.run_swaps();
        let j = rec.swaps();
        assert!(j.len() == 1 && j[0].tries == 1 && j[0].next > now_unix(), "kept for later after a failure: {j:?}");
        println!("PASS failed shrink kept for later (try {} at +{} s)", j[0].tries, j[0].next - now_unix());
        // ... Echo is closed and opened again: a new Recorder on the same folders picks the job up
        std::env::set_var("ECHO_CLOUD_API", &api);
        drop(rec);
        let rec = mk(&api);
        let mut v = rec.swaps();
        v[0].next = 0;   // (its retry time has come)
        rec.save_swaps(&v);
        SWAPS_DUE.store(0, Ordering::Relaxed);
        assert!(rec.swaps_waiting(), "waiting after the restart");
        rec.run_swaps();
        assert!(rec.swaps().is_empty(), "done after the restart");
        let small = r2_size("long.mp4").unwrap();
        assert!(small < long_size, "small copy in place");
        let p = Path::new(&dir).join("served.tmp");
        crate::net::download(&format!("{url}.mp4"), &p).unwrap();
        assert_eq!(fs::metadata(&p).unwrap().len(), small, "link plays it");
        let _ = fs::remove_file(&p);
        println!("PASS resumed after a restart: {:.1} -> {:.1} MB", long_size as f64 / 1e6, small as f64 / 1e6);

        // 2. the sweep: (a) an original kept by the server after an interrupted share first, (b) an upload from an older
        // Echo (no fmt, no fingerprint), (c) an original whose clip isn't on this PC
        let legacy = Path::new(&dir).join("legacy.mp4");
        let legacy_size = fs::metadata(&legacy).unwrap().len();
        let key_l = "Echo/2026/11/11/legacy.mp4";
        let j = ec_call(&api, "POST", "/upload", "tok_harness_test", Some(serde_json::json!({ "path": key_l, "size": legacy_size }))).unwrap();
        crate::net::put_file(j["url"].as_str().unwrap(), &legacy, |_, _| {}, || false).unwrap();
        // (a): another long clip (long_b.mp4, a 150 s cut), its shrink never comes (the job is dropped), the server keeps it
        let long2 = Path::new(&dir).join("long_b.mp4");
        let long_size = fs::metadata(&long2).unwrap().len();
        rec.upload_and_link(&long2.to_string_lossy(), Some(0), &|_| {}).expect("link b");
        rec.save_swaps(&[]);
        crate::net::request("GET", &format!("{api}/__clock?add={}", 2 * 86400), vec![], None, 10).unwrap();
        crate::net::request("GET", &format!("{api}/__cron"), vec![], None, 120).unwrap();
        // (c): an original only in the cloud
        let key_c = "Echo/2026/11/11/gone.mp4";
        let j = ec_call(&api, "POST", "/upload", "tok_harness_test", Some(serde_json::json!({ "path": key_c, "size": 1000, "fmt": "orig" }))).unwrap();
        let tmp1k = Path::new(&dir).join("1k.bin"); fs::write(&tmp1k, vec![0u8; 1000]).unwrap();
        crate::net::put_file(j["url"].as_str().unwrap(), &tmp1k, |_, _| {}, || false).unwrap();
        let _ = fs::remove_file(&tmp1k);
        let mut st = rec.cloud_state();
        for (rel, key, size) in [("legacy.mp4", key_l, legacy_size), ("gone.mp4", key_c, 1000)] {
            st.files.retain(|f| f.rel != rel);
            st.files.push(CloudFile { rel: rel.into(), path: key.trim_start_matches("Echo/").into(), name: rel.trim_end_matches(".mp4").into(), folder: String::new(), size, modified: 0, local: rel != "gone.mp4" });
        }
        rec.save_cloud_state(&st);
        assert!(rec.heal_due(), "sweep due (never ran)");
        let fmt = |k: &str| ec_call(&api, "POST", "/objects/fmt", "tok_harness_test", Some(serde_json::json!({ "paths": [k] }))).unwrap()["files"][k]["fmt"].clone();
        let key_b = format!("Echo/{}", rec.cloud_state().files.iter().find(|f| f.rel == "long_b.mp4").unwrap().path);
        assert_eq!(fmt(&key_b), "orig", "server kept long_b as the original");
        assert_eq!(r2_size("long_b.mp4"), Some(long_size));
        rec.heal_originals(&|| true);
        assert!(!rec.heal_due(), "not due again today");
        let b = r2_size("long_b.mp4").unwrap();
        let l = r2_size("legacy.mp4").unwrap();
        assert!(b < long_size && fmt(&key_b) == "small", "kept original made smaller");
        assert!(l < legacy_size && fmt(key_l) == "small", "older Echo's upload made smaller");
        assert_eq!(r2_size("gone.mp4"), Some(1000), "cloud-only original left alone");
        println!("PASS sweep: long_b {:.1} -> {:.1} MB, legacy {:.1} -> {:.1} MB, cloud-only original left", long_size as f64 / 1e6, b as f64 / 1e6, legacy_size as f64 / 1e6, l as f64 / 1e6);
    }

    /// QA E4 (11 Nov 2026): Echo comes back days after a share-first upload, when the server has already kept the
    /// original (and, on an older server, forgot the swap). The clip and its link must stay. Local test server only.
    /// Needs ECHO_CLOUD_API, ECHO_SF_DIR (long.mp4 + long_b.mp4: two different H.264 clips over 2 min), APPDATA + TEMP.
    /// cargo test -p echo-core --release --lib late_return_end_to_end -- --ignored --nocapture
    #[test]
    #[ignore]
    fn late_return_end_to_end() {
        let (Ok(dir), Ok(api)) = (std::env::var("ECHO_SF_DIR"), std::env::var("ECHO_CLOUD_API")) else { return };
        let mut cfg = crate::settings::Settings::default();
        cfg.clips_dir = dir.clone();
        cfg.cloud = "echocloud".into();
        cfg.cloud_accounts = vec![crate::settings::CloudAccount { id: 1, provider: "echocloud".into(), label: "Echo Cloud".into(), root: String::new() }];
        cfg.ec_token = "tok_harness_test".into();
        cfg.ec_api = api.clone();
        let rec = Recorder::new(cfg);
        let req = |p: &str| { crate::net::request("GET", &format!("{api}{p}"), vec![], None, 120).unwrap(); };
        let state = || crate::net::get_json(&format!("{api}/__state")).unwrap();
        let r2_size = |end: &str| state()["r2"].as_array().unwrap().iter().find(|o| o["key"].as_str().unwrap().ends_with(end)).and_then(|o| o["size"].as_u64());
        let served = |url: &str| { let p = Path::new(&dir).join("served.tmp"); crate::net::download(&format!("{url}.mp4"), &p).unwrap(); let n = fs::metadata(&p).unwrap().len(); let _ = fs::remove_file(&p); n };
        let a = Path::new(&dir).join("long.mp4");
        let b = Path::new(&dir).join("long_b.mp4");
        let (a_size, b_size) = (fs::metadata(&a).unwrap().len(), fs::metadata(&b).unwrap().len());
        // both shared first; Echo is "closed" before their shrinks (the jobs stay in swaps.json)
        let url_a = rec.upload_and_link(&a.to_string_lossy(), Some(0), &|_| {}).expect("link a");
        let url_b = rec.upload_and_link(&b.to_string_lossy(), Some(0), &|_| {}).expect("link b");
        assert_eq!(rec.swaps().len(), 2);
        // 1.5 days later the server's daily job has kept both originals
        req(&format!("/__clock?add={}", 36 * 3600));
        req("/__cron");
        assert!(state()["swaps"].as_array().unwrap().iter().all(|s| s["done"].as_u64().unwrap_or(0) > 0), "server finished both");
        // b: an older server has also forgotten the swap by now (the case that deleted the clip): like its daily job,
        // the hidden original goes, then the row
        let tmp_b = state()["swaps"].as_array().unwrap().iter().find(|s| s["key"].as_str().unwrap().ends_with("long_b.mp4"))
            .map(|s| s["tmp"].as_str().unwrap().rsplit('/').next().unwrap().to_string()).unwrap();
        req(&format!("/__r2del?prefix={tmp_b}"));
        let n = crate::net::request("POST", &format!("{api}/__sql"), vec![], Some("DELETE FROM swaps WHERE key LIKE '%long_b.mp4'".into()), 10).unwrap();
        assert_eq!(n.1.trim(), "1");
        // Echo starts again
        let mut v = rec.swaps();
        for j in &mut v { j.next = 0; j.created = now_unix(); }
        rec.save_swaps(&v);
        SWAPS_DUE.store(0, Ordering::Relaxed);
        rec.run_swaps();
        assert!(rec.swaps().is_empty(), "jobs done");
        assert_eq!(r2_size("long.mp4"), Some(a_size), "a: the kept original is still there");
        assert_eq!(r2_size("long_b.mp4"), Some(b_size), "b: the kept original is still there (not deleted)");
        assert_eq!(served(&url_a), a_size, "a: link plays");
        assert_eq!(served(&url_b), b_size, "b: link plays");
        println!("PASS late return: both clips + links kept ({:.1} / {:.1} MB originals, the sweep makes them smaller)", a_size as f64 / 1e6, b_size as f64 / 1e6);
        // the server keeps a finished swap's row for days (hidden original gone after 6 h)
        req(&format!("/__clock?add={}", 2 * 86400));
        req("/__cron");
        let s = state()["swaps"].clone();
        assert!(s.as_array().unwrap().iter().any(|x| x["key"].as_str().unwrap().ends_with("long.mp4") && x["tmp"] == ""), "row kept, original gone: {s}");
        assert!(!state()["r2"].as_array().unwrap().iter().any(|o| o["key"].as_str().unwrap().contains("/.swap/")), "hidden originals deleted");
        assert_eq!(served(&url_a), a_size, "a: link still plays after the grace cleanup");
        req(&format!("/__clock?add={}", 3 * 86400));
        req("/__cron");
        assert!(state()["swaps"].as_array().unwrap().is_empty(), "row gone after 4 days");
        println!("PASS server keeps finished rows 4 days, hidden originals go after 6 h");
    }
}
