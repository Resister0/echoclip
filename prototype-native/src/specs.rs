//! r2-perf (10 Nov 2026): the PC behind the performance numbers (perf.rs). The developer wants to see Echo's CPU / memory
//! next to the hardware it ran on. Model names and recording settings only - nothing about the person, no ID.
//! Hardware (CPU, RAM, GPU, OS) is read once and cached; the recording settings (resolution, fps, encoder) are read
//! at each report, they change with Settings > Video. perf::query() appends `q()` to the update check.
use echo_core::settings::Settings;
use std::sync::OnceLock;

struct Hw {
    cpu: String,
    ram_gb: u32,
    gpu: String,
    os: String,
}

static HW: OnceLock<Hw> = OnceLock::new();

/// "&cpu=..&gpu=..&ram=..&os=..&res=..&fps=..&enc=.." (URL-encoded; empty values left out)
pub fn q() -> String {
    // the recorder's settings + the capture mode it started with ("" = not started / another Echo records)
    let rec = crate::rec_core::rec().map(|r| (r.settings(), r.mode.lock().map(|m| m.clone()).unwrap_or_default()));
    q_with(rec.as_ref().map(|(c, m)| (c, m.as_str())))
}

fn q_with(rec: Option<(&Settings, &str)>) -> String {
    let hw = HW.get_or_init(read_hw);
    let mut out = String::new();
    let mut add = |k: &str, v: &str| {
        let v = v.trim();
        if !v.is_empty() {
            out.push('&');
            out.push_str(k);
            out.push('=');
            out.push_str(&enc(&v.chars().take(80).collect::<String>()));
        }
    };
    add("cpu", &hw.cpu);
    add("gpu", &hw.gpu);
    add("ram", &if hw.ram_gb > 0 { hw.ram_gb.to_string() } else { String::new() });
    add("os", &hw.os);
    let (res, fps, encoder) = rec.map(|(cfg, mode)| recording(cfg, mode)).unwrap_or_default();
    add("res", &res);
    add("fps", &if fps > 0 { fps.to_string() } else { String::new() });
    add("enc", &encoder);
    out
}

/// percent-encoding for a query value (RFC 3986 unreserved characters stay)
fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// "AMD Ryzen 7 5800X 8-Core Processor" - the long runs of spaces some CPUs report squeezed out
fn tidy(s: &str) -> String {
    s.replace("(R)", "").replace("(TM)", "").replace("(tm)", "").replace("(r)", "").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(not(windows))]
fn vendor_name(v: u32) -> &'static str {
    match v {
        0x1002 | 0x1022 => "AMD",
        0x10DE => "NVIDIA",
        0x8086 => "Intel",
        _ => "",
    }
}

fn codec_name(c: &str) -> &'static str {
    match c {
        "hevc" => "HEVC",
        "av1" => "AV1",
        _ => "H.264",
    }
}

/// output size from the monitor's size and Settings > Video > Resolution ("native" or a height like "1080")
fn out_size(w: u32, h: u32, setting: &str) -> String {
    match setting.parse::<u32>() {
        Ok(th) if w > 0 && h > 0 => {
            // ffmpeg scale=-2:h (Windows) / gpu-screen-recorder: same aspect, even width
            let tw = ((w as f64 * th as f64 / h as f64 / 2.0).round() as u32) * 2;
            format!("{tw}x{th}")
        }
        Ok(th) => format!("{}x{th}", th * 16 / 9),
        Err(_) if w > 0 && h > 0 => format!("{w}x{h}"),
        Err(_) => String::new(),
    }
}

// ---------------- Windows ----------------

#[cfg(windows)]
fn reg_str(key: &str, val: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (k, v) = (w(key), w(val));
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _, &mut size) };
    if r != 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]).trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(windows)]
fn read_hw() -> Hw {
    let cpu = reg_str(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString").map(|s| tidy(&s)).unwrap_or_default();
    let ram_gb = unsafe {
        use windows_sys::Win32::System::SystemInformation::{GetPhysicallyInstalledSystemMemory, GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut kb = 0u64;
        if GetPhysicallyInstalledSystemMemory(&mut kb) != 0 && kb > 0 {
            (kb as f64 / 1048576.0).round() as u32
        } else {
            let mut m: MEMORYSTATUSEX = std::mem::zeroed();
            m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut m) != 0 { (m.ullTotalPhys as f64 / 1073741824.0).ceil() as u32 } else { 0 }
        }
    };
    // "Windows 11 24H2 (26100)": ProductName still says "Windows 10" on 11 - the build number decides
    let nt = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build: u32 = reg_str(nt, "CurrentBuildNumber").and_then(|b| b.parse().ok()).unwrap_or(0);
    let os = if build >= 10240 {
        let name = if build >= 22000 { "Windows 11" } else { "Windows 10" };
        let ver = reg_str(nt, "DisplayVersion").or_else(|| reg_str(nt, "ReleaseId")).unwrap_or_default();
        format!("{name} {ver} ({build})").replace("  ", " ")
    } else {
        reg_str(nt, "ProductName").unwrap_or_default()
    };
    Hw { cpu, ram_gb, gpu: gpu_name(), os }
}

/// the GPU Echo records with: the one driving the recorded monitor (DXGI vendor), its name from the display
/// adapters' registry keys (DriverDesc); else the first real (PCI) adapter
#[cfg(windows)]
fn gpu_name() -> String {
    let monitor = crate::rec_core::rec().map(|r| r.settings().monitor).unwrap_or_default();
    let want = echo_core::gpucap::monitor_vendor(&monitor);
    let class = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let mut first = None;
    for i in 0..32 {
        let key = format!(r"{class}\{i:04}");
        let Some(desc) = reg_str(&key, "DriverDesc") else { continue };
        let id = reg_str(&key, "MatchingDeviceId").unwrap_or_default().to_ascii_lowercase();
        // virtual / remote adapters (Basic Display, Parsec, Meta Virtual Monitor, RDP) aren't on PCI
        if !id.starts_with("pci\\") {
            continue;
        }
        let ven = id.split("ven_").nth(1).and_then(|v| u32::from_str_radix(v.get(..4)?, 16).ok()).unwrap_or(0);
        if want != 0 && ven == want {
            return tidy(&desc);
        }
        first.get_or_insert(desc);
    }
    first.map(|d| tidy(&d)).unwrap_or_default()
}

/// (output size, fps, encoder) of the recorder as it runs now
#[cfg(windows)]
fn recording(cfg: &Settings, mode: &str) -> (String, u32, String) {
    let mons = echo_core::gpucap::list_monitors();
    let m = mons.iter().find(|m| !cfg.monitor.is_empty() && m.name == cfg.monitor).or_else(|| mons.iter().find(|m| m.primary)).or(mons.first());
    let mut res = m.map(|m| out_size(m.width, m.height, &cfg.resolution)).unwrap_or_default();
    if cfg.capture == "game" && !res.is_empty() {
        res.push_str(" (game window)");
    }
    let codec = codec_name(&cfg.codec);
    // "gpu" = Echo's zero-copy pipeline with the capture GPU's own Media Foundation encoder; "cpu*" = ffmpeg fallbacks
    let enc = match mode {
        "gpu" => {
            let v = echo_core::gpucap::monitor_vendor(&cfg.monitor);
            let e = match v {
                0x1002 => "AMD AMF",
                0x10DE => "NVIDIA NVENC",
                0x8086 => "Intel Quick Sync",
                _ => "GPU",
            };
            format!("{e} {codec}")
        }
        "cpu" => format!("AMD AMF {codec} (CPU copy)"),
        "cpu-nvenc" => format!("NVIDIA NVENC {codec} (CPU copy)"),
        "cpu-qsv" => format!("Intel Quick Sync {codec} (CPU copy)"),
        _ => String::new(),
    };
    (res, cfg.fps, enc)
}

// ---------------- Linux ----------------

#[cfg(not(windows))]
fn read(p: &str) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

#[cfg(not(windows))]
fn read_hw() -> Hw {
    let cpu = read("/proc/cpuinfo").lines().find(|l| l.starts_with("model name")).and_then(|l| l.split_once(':')).map(|(_, v)| tidy(v)).unwrap_or_default();
    // MemTotal is a bit under the installed RAM (the kernel's own share): 31.2 GB -> 32
    let kb: f64 = read("/proc/meminfo").lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let ram_gb = (kb / 1048576.0).ceil() as u32;
    Hw { cpu, ram_gb, gpu: gpu_linux().1, os: os_linux() }
}

/// "Fedora Linux 43 (KDE Plasma, Wayland)" - the Flatpak's /etc/os-release is the runtime's: the host's is in /run/host
#[cfg(not(windows))]
fn os_linux() -> String {
    let rel = if std::path::Path::new("/run/host/os-release").exists() { read("/run/host/os-release") } else { read("/etc/os-release") };
    let field = |k: &str| {
        rel.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(|v| v.trim().trim_matches('"').to_string()).filter(|v| !v.is_empty())
    };
    let base = match (field("NAME"), field("VERSION_ID")) {
        (Some(n), Some(v)) => format!("{n} {v}"),
        _ => field("PRETTY_NAME").unwrap_or_else(|| "Linux".into()),
    };
    // XDG_CURRENT_DESKTOP is a list ("ubuntu:GNOME"): the last entry is the desktop itself
    let de = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let de = de.split(':').filter(|s| !s.is_empty()).last().unwrap_or("").trim_start_matches("X-").to_string();
    let de = match de.as_str() {
        "KDE" => "KDE Plasma".to_string(),
        _ => de,
    };
    let session = match std::env::var("XDG_SESSION_TYPE").unwrap_or_default().as_str() {
        "wayland" => "Wayland",
        "x11" => "X11",
        _ => "",
    };
    let extra: Vec<&str> = [de.as_str(), session].into_iter().filter(|s| !s.is_empty()).collect();
    if extra.is_empty() { base } else { format!("{base} ({})", extra.join(", ")) }
}

/// (PCI vendor, name) of the GPU driving a connected screen (else the first GPU): /sys/class/drm + the PCI ID list
/// (hwdata) for the model name, "AMD 1002:744c" when the list isn't there (Flatpak runtime)
#[cfg(not(windows))]
fn gpu_linux() -> (u32, String) {
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else { return (0, String::new()) };
    let mut names: Vec<String> = dir.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    let cards: Vec<&String> = names.iter().filter(|n| n.starts_with("card") && !n.contains('-')).collect();
    let connected = |card: &str| names.iter().any(|n| n.starts_with(&format!("{card}-")) && read(&format!("/sys/class/drm/{n}/status")).trim() == "connected");
    let hex = |p: String| u32::from_str_radix(read(&p).trim().trim_start_matches("0x"), 16).unwrap_or(0);
    let pick = cards.iter().find(|c| connected(c)).or(cards.first());
    let Some(card) = pick else { return (0, String::new()) };
    let ven = hex(format!("/sys/class/drm/{card}/device/vendor"));
    let dev = hex(format!("/sys/class/drm/{card}/device/device"));
    if ven == 0 {
        return (0, String::new());
    }
    let vn = vendor_name(ven);
    let model = pci_name(ven, dev).unwrap_or_else(|| format!("{ven:04x}:{dev:04x}"));
    // the PCI list often names the chip ("Navi 31 [Radeon RX 7900 XT/7900 XTX/7900M]"): the bracket is the product
    let model = match (model.find('['), model.rfind(']')) {
        (Some(a), Some(b)) if b > a => model[a + 1..b].to_string(),
        _ => model,
    };
    let name = if vn.is_empty() || model.starts_with(vn) { model } else { format!("{vn} {model}") };
    (ven, name)
}

#[cfg(not(windows))]
fn pci_name(ven: u32, dev: u32) -> Option<String> {
    let ids = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids", "/run/host/usr/share/hwdata/pci.ids"].iter().map(|p| read(p)).find(|s| !s.is_empty())?;
    let (v, d) = (format!("{ven:04x}  "), format!("\t{dev:04x}  "));
    let mut in_vendor = false;
    for l in ids.lines() {
        if !l.starts_with('\t') && !l.starts_with('#') {
            if in_vendor {
                return None;
            }
            in_vendor = l.starts_with(&v);
        } else if in_vendor {
            if let Some(n) = l.strip_prefix(&d) {
                return Some(n.trim().to_string());
            }
        }
    }
    None
}

/// first connected screen's size from /sys/class/drm (its preferred mode is the first line of "modes")
#[cfg(not(windows))]
fn screen_linux() -> (u32, u32) {
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else { return (0, 0) };
    let mut names: Vec<String> = dir.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.contains('-')).collect();
    names.sort();
    for n in names {
        if read(&format!("/sys/class/drm/{n}/status")).trim() != "connected" {
            continue;
        }
        let m = read(&format!("/sys/class/drm/{n}/modes"));
        if let Some((w, h)) = m.lines().next().and_then(|l| l.split_once('x')) {
            let h: String = h.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                return (w, h);
            }
        }
    }
    (0, 0)
}

/// (output size, fps, encoder): gpu-screen-recorder encodes with VAAPI (AMD / Intel) or NVENC (NVIDIA)
#[cfg(not(windows))]
fn recording(cfg: &Settings, mode: &str) -> (String, u32, String) {
    let (w, h) = screen_linux();
    let res = match cfg.resolution.parse::<u32>() {
        // Echo asks gpu-screen-recorder for -s (h*16/9)x(h)
        Ok(th) => format!("{}x{th}", th * 16 / 9),
        Err(_) => out_size(w, h, "native"),
    };
    let enc = if mode.is_empty() {
        String::new()
    } else {
        let ven = gpu_linux().0;
        let api = if ven == 0x10DE { "NVENC" } else { "VAAPI" };
        let vn = vendor_name(ven);
        format!("{}{api} {}", if vn.is_empty() { String::new() } else { format!("{vn} ") }, codec_name(&cfg.codec))
    };
    (res, cfg.fps, enc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_sizes() {
        assert_eq!(enc("Windows 11 24H2 (26100)"), "Windows%2011%2024H2%20%2826100%29");
        assert_eq!(enc("AMD AMF H.264"), "AMD%20AMF%20H.264");
        assert_eq!(out_size(2560, 1440, "1080"), "1920x1080");
        assert_eq!(out_size(3440, 1440, "1080"), "2580x1080");
        assert_eq!(out_size(1920, 1080, "native"), "1920x1080");
        assert_eq!(tidy("Intel(R) Core(TM) i7-9700K CPU @ 3.60GHz  "), "Intel Core i7-9700K CPU @ 3.60GHz");
    }

    /// prints the whole update-check URL this PC would send, with its real settings.json (read only) and the recorder
    /// in its zero-copy mode (cargo test --bin echo-native-proto specs -- --nocapture)
    #[test]
    fn this_pc() {
        let cfg: Settings = std::fs::read_to_string(Settings::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let s = q_with(Some((&cfg, "gpu")));
        let os = if cfg!(windows) { "windows" } else { "linux" };
        println!("URL: https://echoclip.link/update/{os}/x86_64/1.0.71?m=212&mp=260&c=8&cp=31&rm=240&rc=14&n=180&rn=60&k=16{s}");
        assert!(s.contains("&cpu=") && s.contains("&os=") && s.contains("&fps=") && s.contains("&enc="));
        // not recording yet (no recorder / other Echo): hardware only
        assert!(!q().contains("&enc="));
    }
}
