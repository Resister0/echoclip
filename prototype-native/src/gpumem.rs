//! 12 Nov 2026: Echo's own graphics memory (VRAM) for the performance numbers (perf.rs): how much of the graphics
//! card's own memory the processes hold right now - what a game running next to Echo can't use. Summed over every
//! GPU in the PC (a graphics card + the processor's own graphics = two). Read with D3DKMTQueryStatistics (the kernel
//! call behind Task Manager's GPU columns), "local segment group" usage per process: the same number as the
//! "GPU Process Memory \ Local Usage" counter and DXGI's QueryVideoMemoryInfo (checked on an AMD card: all three
//! 456.8 MB). ~2 kernel calls per GPU per process once a minute: no GPU work, no new thread.
//! Not the same as the "Dedicated Usage" counter: that one also counts memory set aside for the card that isn't on it
//! right now (538 vs 457 MB on that PC).

/// MB of graphics-card memory these processes hold together (Windows); None = couldn't read it for any of them
#[cfg(windows)]
pub fn vram_mb(pids: &[u32]) -> Option<f64> {
    use windows_sys::Wdk::Graphics::Direct3D::*;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION};
    unsafe {
        // the first call says how many GPUs there can be, the second fills the list
        let mut e: D3DKMT_ENUMADAPTERS2 = std::mem::zeroed();
        if D3DKMTEnumAdapters2(&mut e) < 0 || e.NumAdapters == 0 {
            return None;
        }
        let mut list: Vec<D3DKMT_ADAPTERINFO> = vec![std::mem::zeroed(); e.NumAdapters as usize];
        e.pAdapters = list.as_mut_ptr();
        if D3DKMTEnumAdapters2(&mut e) < 0 {
            return None;
        }
        list.truncate(e.NumAdapters as usize);
        let mut bytes = 0u64;
        let mut read_any = false;
        for &pid in pids {
            // a "limited" handle is refused by this query (STATUS_INVALID_PARAMETER); query rights only, no memory access
            let h = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
            if h.is_null() {
                continue;
            }
            for a in &list {
                let mut s: D3DKMT_QUERYSTATISTICS = std::mem::zeroed();
                s.Type = D3DKMT_QUERYSTATISTICS_PROCESS_SEGMENT_GROUP;
                s.AdapterLuid = a.AdapterLuid;
                s.hProcess = h;
                s.Anonymous.QueryProcessSegmentGroup = D3DKMT_MEMORY_SEGMENT_GROUP_LOCAL;
                // GPUs this process never used (or a display-only adapter) answer with an error: nothing to add
                if D3DKMTQueryStatistics(&s) >= 0 {
                    bytes += s.QueryResult.ProcessSegmentGroupInformation.Usage;
                    read_any = true;
                }
            }
            CloseHandle(h);
        }
        read_any.then_some(bytes as f64 / 1048576.0)
    }
}

/// Linux: not measured - perf.rs sends no VRAM fields. (amdgpu / i915 show it per open DRM file in
/// /proc/<pid>/fdinfo as drm-memory-vram, but Echo's GL / mpv / VA-API each open their own render node and the same
/// buffer can show up in several of them - not cheap to get right without double counting.)
#[cfg(not(windows))]
pub fn vram_mb(_pids: &[u32]) -> Option<f64> {
    None
}
