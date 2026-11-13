//! Zero-copy screen recorder (Windows):
//! DXGI Desktop Duplication (GPU texture) -> D3D11 video processor BGRA->NV12 (GPU)
//! -> Media Foundation hardware H.264 encoder (AMD/NVIDIA/Intel MFT, async) -> named pipe -> ffmpeg (mux only).
//! Frames never leave the GPU until they are compressed.
//! Also: any monitor, one window only (Windows.Graphics.Capture), HDR monitors (scRGB -> SDR shader),
//! H.264 / HEVC / AV1, and the encoder is picked on the capture GPU (adapter LUID) so NVIDIA works too.

use std::mem::ManuallyDrop;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
#[allow(unused_imports)]
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Pipes::*;
use windows::core::VARIANT;

pub type Log = fn(&str);

/// Owned named-pipe server handle (raw value so it can cross threads).
pub struct Pipe(pub isize);

pub fn create_pipe(name: &str) -> Result<Pipe, String> {
    let h = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(name),
            PIPE_ACCESS_OUTBOUND,
            PIPE_TYPE_BYTE | PIPE_WAIT,
            1,
            4 << 20,
            4 << 20,
            0,
            None,
        )
    };
    if h.is_invalid() {
        return Err(format!("CreateNamedPipe failed: {:?}", unsafe { GetLastError() }));
    }
    Ok(Pipe(h.0 as isize))
}

/// Live counters for the health check: frames sent to the encoder, and frames that were late
/// (the capture loop fell more than 2 frames behind real time, i.e. the GPU / game was too busy).
pub static FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static LATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn e<E: std::fmt::Display>(ctx: &'static str) -> impl FnOnce(E) -> String {
    move |err| format!("{ctx}: {err}")
}

/// What to record and how (from the app settings).
#[derive(Clone)]
pub struct Opts {
    pub fps: u32,
    pub bitrate_mbps: u32,
    pub gop: u32,
    /// 0 = native height
    pub target_h: u32,
    /// monitor device name ("\\.\DISPLAY2"); "" = primary
    pub monitor: String,
    /// "h264" | "hevc" | "av1"
    pub codec: String,
    /// smart bitrate: constant quality capped at bitrate_mbps (calm moments use far fewer bits), else CBR
    pub smart_bitrate: bool,
    /// 0-100, higher = sharper
    pub smart_quality: u32,
    /// window to record (HWND as isize); 0 = whole monitor
    pub window: isize,
    /// draw the mouse pointer into the video (Desktop Duplication leaves it out; games that hide it stay clean)
    pub cursor: bool,
}

/// A monitor for the settings list.
pub struct MonitorInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub hdr: bool,
}

struct OutputSel {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput1,
    width: u32,
    height: u32,
    hdr: bool,
    name: String,
    /// index of the output on its adapter (ffmpeg ddagrab output_idx)
    idx: u32,
}

fn wide(s: &[u16]) -> String {
    let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..n])
}

fn all_outputs() -> Result<Vec<(OutputSel, bool)>, String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(e("CreateDXGIFactory1"))?;
        let mut v = Vec::new();
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(out) = adapter.EnumOutputs(o) {
                o += 1;
                let Ok(desc) = out.GetDesc() else { continue };
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                let r = desc.DesktopCoordinates;
                let (w, h) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
                let Ok(out1) = out.cast::<IDXGIOutput1>() else { continue };
                let hdr = out
                    .cast::<IDXGIOutput6>()
                    .ok()
                    .and_then(|o6| o6.GetDesc1().ok())
                    .map_or(false, |d| d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
                let primary = r.left == 0 && r.top == 0;
                v.push((OutputSel { adapter: adapter.clone(), output: out1, width: w, height: h, hdr, name: wide(&desc.DeviceName), idx: o - 1 }, primary));
            }
            a += 1;
        }
        Ok(v)
    }
}

/// Monitors for Settings > Video.
pub fn list_monitors() -> Vec<MonitorInfo> {
    all_outputs()
        .unwrap_or_default()
        .into_iter()
        .map(|(o, primary)| MonitorInfo { name: o.name, width: o.width, height: o.height, primary, hdr: o.hdr })
        .collect()
}

/// PCI vendor of the GPU that drives a monitor (0x1002 AMD, 0x10DE NVIDIA, 0x8086 Intel; 0 = unknown).
/// The CPU fallback tries that vendor's ffmpeg encoder first (it used to try AMD's on every PC).
pub fn monitor_vendor(name: &str) -> u32 {
    pick_output(name).ok().and_then(|o| unsafe { o.adapter.GetDesc1().ok() }).map_or(0, |d| d.VendorId)
}

/// ddagrab output index of a monitor (CPU fallback modes).
pub fn output_index(name: &str) -> u32 {
    pick_output(name).map(|o| o.idx).unwrap_or(0)
}

/// The chosen monitor (by device name), else the primary one, else the first.
fn pick_output(name: &str) -> Result<OutputSel, String> {
    let mut v = all_outputs()?;
    if let Some(i) = v.iter().position(|(o, _)| !name.is_empty() && o.name.eq_ignore_ascii_case(name)) {
        return Ok(v.swap_remove(i).0);
    }
    if let Some(i) = v.iter().position(|(_, p)| *p) {
        return Ok(v.swap_remove(i).0);
    }
    v.into_iter().next().map(|x| x.0).ok_or_else(|| "No monitor found".to_string())
}

/// Windows "SDR content brightness" for an HDR monitor, as a multiplier from scRGB to SDR (1.0 = 80 nits).
fn sdr_scale(device_name: &str) -> f32 {
    use windows::Win32::Devices::Display::*;
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return 0.4;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
            return 0.4;
        }
        for p in &paths[..np as usize] {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 || !wide(&src.viewGdiDeviceName).eq_ignore_ascii_case(device_name) {
                continue;
            }
            let mut wl = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
            wl.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
            wl.header.size = std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32;
            wl.header.adapterId = p.targetInfo.adapterId;
            wl.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut wl.header) == 0 && wl.SDRWhiteLevel > 0 {
                // SDRWhiteLevel: 1000 = 80 nits
                return (1000.0 / wl.SDRWhiteLevel as f32).clamp(0.05, 1.0);
            }
        }
        0.4
    }
}

/// HDR desktop (scRGB half-float) -> SDR BGRA, drawn with a tiny shader so colours don't come out washed out.
struct HdrConv {
    fp16: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    rtv: ID3D11RenderTargetView,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    samp: ID3D11SamplerState,
    cb: ID3D11Buffer,
    w: u32,
    h: u32,
}

const HDR_HLSL: &str = r#"
Texture2D t : register(t0); SamplerState s : register(s0);
cbuffer C : register(b0) { float scale; float3 pad; };
struct V { float4 p : SV_Position; float2 uv : TEXCOORD0; };
V vs(uint id : SV_VertexID) { V o; o.uv = float2((id << 1) & 2, id & 2); o.p = float4(o.uv * float2(2, -2) + float2(-1, 1), 0, 1); return o; }
float3 enc(float3 c) { return c <= 0.0031308 ? c * 12.92 : 1.055 * pow(abs(c), 1.0 / 2.4) - 0.055; }
float4 ps(V i) : SV_Target {
    float3 c = max(t.Sample(s, i.uv).rgb * scale, 0);
    float m = max(c.r, max(c.g, c.b));
    if (m > 0.8) { float n = 0.8 + 0.2 * (1 - exp(-(m - 0.8) / 0.2)); c *= n / m; }   // soft roll-off of HDR highlights
    return float4(enc(saturate(c)), 1);
}
"#;

unsafe fn compile(src: &str, entry: &str, target: &str) -> Result<ID3DBlob, String> {
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    let mut code = None;
    let mut errs = None;
    let entry = std::ffi::CString::new(entry).unwrap();
    let target = std::ffi::CString::new(target).unwrap();
    let r = D3DCompile(
        src.as_ptr() as *const _,
        src.len(),
        windows::core::PCSTR::null(),
        None,
        None,
        windows::core::PCSTR(entry.as_ptr() as *const u8),
        windows::core::PCSTR(target.as_ptr() as *const u8),
        0,
        0,
        &mut code,
        Some(&mut errs),
    );
    if r.is_err() {
        let msg = errs
            .map(|b: ID3DBlob| String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).to_string())
            .unwrap_or_default();
        return Err(format!("shader: {msg}"));
    }
    code.ok_or_else(|| "shader: no code".into())
}

unsafe fn blob(b: &ID3DBlob) -> &[u8] {
    std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())
}

impl HdrConv {
    fn new(device: &ID3D11Device, bgra: &ID3D11Texture2D, w: u32, h: u32, scale: f32) -> Result<Self, String> {
        unsafe {
            let td = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut fp16 = None;
            device.CreateTexture2D(&td, None, Some(&mut fp16)).map_err(e("CreateTexture2D fp16"))?;
            let fp16 = fp16.ok_or("no fp16 tex")?;
            let mut srv = None;
            device.CreateShaderResourceView(&fp16, None, Some(&mut srv)).map_err(e("SRV"))?;
            let mut rtv = None;
            device.CreateRenderTargetView(bgra, None, Some(&mut rtv)).map_err(e("RTV"))?;
            let vsb = compile(HDR_HLSL, "vs", "vs_4_0")?;
            let psb = compile(HDR_HLSL, "ps", "ps_4_0")?;
            let mut vs = None;
            device.CreateVertexShader(blob(&vsb), None, Some(&mut vs)).map_err(e("VS"))?;
            let mut ps = None;
            device.CreatePixelShader(blob(&psb), None, Some(&mut ps)).map_err(e("PS"))?;
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut samp = None;
            device.CreateSamplerState(&sd, Some(&mut samp)).map_err(e("sampler"))?;
            let data = [scale, 0.0, 0.0, 0.0];
            let bd = D3D11_BUFFER_DESC {
                ByteWidth: 16,
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr() as *const _, SysMemPitch: 0, SysMemSlicePitch: 0 };
            let mut cb = None;
            device.CreateBuffer(&bd, Some(&init), Some(&mut cb)).map_err(e("cbuffer"))?;
            Ok(HdrConv {
                fp16,
                srv: srv.ok_or("no srv")?,
                rtv: rtv.ok_or("no rtv")?,
                vs: vs.ok_or("no vs")?,
                ps: ps.ok_or("no ps")?,
                samp: samp.ok_or("no sampler")?,
                cb: cb.ok_or("no cbuffer")?,
                w,
                h,
            })
        }
    }

    /// fp16 (already filled) -> bgra
    fn draw(&self, ctx: &ID3D11DeviceContext) {
        unsafe {
            ctx.OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
            let vp = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: self.w as f32, Height: self.h as f32, MinDepth: 0.0, MaxDepth: 1.0 };
            ctx.RSSetViewports(Some(&[vp]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(self.srv.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.samp.clone())]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.Draw(3, 0);
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
        }
    }
}

const CURSOR_HLSL: &str = r#"
Texture2D t : register(t0); SamplerState s : register(s0);
struct V { float4 p : SV_Position; float2 uv : TEXCOORD0; };
V vs(uint id : SV_VertexID) { V o; o.uv = float2((id << 1) & 2, id & 2); o.p = float4(o.uv * float2(2, -2) + float2(-1, 1), 0, 1); return o; }
float4 ps(V i) : SV_Target { return t.Sample(s, i.uv); }
"#;

/// The mouse pointer on top of whole-monitor recordings. Desktop Duplication hands it over separately
/// (position + shape); it is drawn onto a copy of the frame right before the NV12 conversion, so the
/// captured frame itself stays clean when only the mouse moves. Invert-style pointers (text I-beam)
/// are drawn as a real colour inversion with a second blend pass.
struct CursorDraw {
    comp: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    view: ID3D11VideoProcessorInputView,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    samp: ID3D11SamplerState,
    blend: ID3D11BlendState,
    blend_inv: ID3D11BlendState,
    opaque: ID3D11BlendState,
    /// normal pixels (straight alpha) and "invert the screen here" pixels
    norm: Option<ID3D11ShaderResourceView>,
    inv: Option<ID3D11ShaderResourceView>,
    w: u32,
    h: u32,
    x: i32,
    y: i32,
    visible: bool,
    buf: Vec<u8>,
    /// screen size + since when the pointer sits (still) in the middle: games that lock a visible pointer to the
    /// centre while you play (mouse-look) shouldn't show it (chat 21)
    sw: i32,
    sh: i32,
    mid_since: Option<std::time::Instant>,
}

impl CursorDraw {
    fn new(device: &ID3D11Device, vdev: &ID3D11VideoDevice, venum: &ID3D11VideoProcessorEnumerator, w: u32, h: u32) -> Result<Self, String> {
        let (sw, sh) = (w, h);
        unsafe {
            let comp = bgra_tex(device, w, h)?;
            let mut rtv = None;
            device.CreateRenderTargetView(&comp, None, Some(&mut rtv)).map_err(e("cursor RTV"))?;
            let view = in_view(vdev, venum, &comp)?;
            let vsb = compile(CURSOR_HLSL, "vs", "vs_4_0")?;
            let psb = compile(CURSOR_HLSL, "ps", "ps_4_0")?;
            let mut vs = None;
            device.CreateVertexShader(blob(&vsb), None, Some(&mut vs)).map_err(e("cursor VS"))?;
            let mut ps = None;
            device.CreatePixelShader(blob(&psb), None, Some(&mut ps)).map_err(e("cursor PS"))?;
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut samp = None;
            device.CreateSamplerState(&sd, Some(&mut samp)).map_err(e("cursor sampler"))?;
            let mk = |on: bool, src: D3D11_BLEND, dst: D3D11_BLEND| -> Result<ID3D11BlendState, String> {
                let rt = D3D11_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: on.into(),
                    SrcBlend: src,
                    DestBlend: dst,
                    BlendOp: D3D11_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D11_BLEND_ZERO,
                    DestBlendAlpha: D3D11_BLEND_ONE,
                    BlendOpAlpha: D3D11_BLEND_OP_ADD,
                    RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
                };
                let bd = D3D11_BLEND_DESC { AlphaToCoverageEnable: FALSE, IndependentBlendEnable: FALSE, RenderTarget: [rt; 8] };
                let mut b = None;
                device.CreateBlendState(&bd, Some(&mut b)).map_err(e("blend state"))?;
                b.ok_or_else(|| "no blend state".to_string())
            };
            Ok(CursorDraw {
                comp,
                rtv: rtv.ok_or("no cursor rtv")?,
                view,
                vs: vs.ok_or("no cursor vs")?,
                ps: ps.ok_or("no cursor ps")?,
                samp: samp.ok_or("no cursor sampler")?,
                // out = src * srcA + dst * (1 - srcA)
                blend: mk(true, D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_INV_SRC_ALPHA)?,
                // out = src * (1 - dst) + dst * (1 - srcA): white opaque pixel = inverted screen, clear pixel = unchanged
                blend_inv: mk(true, D3D11_BLEND_INV_DEST_COLOR, D3D11_BLEND_INV_SRC_ALPHA)?,
                opaque: mk(false, D3D11_BLEND_ONE, D3D11_BLEND_ZERO)?,
                norm: None,
                inv: None,
                w: 0,
                h: 0,
                x: 0,
                y: 0,
                visible: false,
                buf: Vec::new(),
                sw: sw as i32,
                sh: sh as i32,
                mid_since: None,
            })
        }
    }

    fn showing(&self) -> bool {
        let parked = self.mid_since.is_some_and(|t| t.elapsed().as_millis() > 1000);
        self.visible && !parked && self.w > 0 && (self.norm.is_some() || self.inv.is_some())
    }

    /// New position / shape from this frame's info (call before ReleaseFrame).
    unsafe fn update(&mut self, device: &ID3D11Device, dup: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO) {
        if info.LastMouseUpdateTime != 0 {
            self.visible = info.PointerPosition.Visible.as_bool();
            self.x = info.PointerPosition.Position.x;
            self.y = info.PointerPosition.Position.y;
            let mid = (self.x - self.sw / 2).abs() <= 8 && (self.y - self.sh / 2).abs() <= 8;
            if !mid {
                self.mid_since = None;
            } else if self.mid_since.is_none() {
                self.mid_since = Some(std::time::Instant::now());
            }
        }
        if info.PointerShapeBufferSize == 0 {
            return;
        }
        self.buf.resize(info.PointerShapeBufferSize as usize, 0);
        let mut need = 0u32;
        let mut si = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        if dup.GetFramePointerShape(self.buf.len() as u32, self.buf.as_mut_ptr() as *mut _, &mut need, &mut si).is_err() {
            return;
        }
        let _ = self.set_shape(device, &si);
    }

    /// Pointer bitmap -> two BGRA textures (normal pixels, inverting pixels).
    unsafe fn set_shape(&mut self, device: &ID3D11Device, si: &DXGI_OUTDUPL_POINTER_SHAPE_INFO) -> Result<(), String> {
        let mono = si.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32;
        let (w, h, pitch) = (si.Width as usize, if mono { si.Height as usize / 2 } else { si.Height as usize }, si.Pitch as usize);
        if w == 0 || h == 0 || w > 256 || h > 256 {
            self.norm = None; self.inv = None; self.w = 0;
            return Ok(());
        }
        let b = &self.buf;
        let mut norm = vec![0u8; w * h * 4];
        let mut inv = vec![0u8; w * h * 4];
        let (mut any_n, mut any_i) = (false, false);
        let px = |v: &mut Vec<u8>, i: usize, bgra: [u8; 4]| v[i * 4..i * 4 + 4].copy_from_slice(&bgra);
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if mono {
                    let bit = 0x80u8 >> (x % 8);
                    let (ai, xi) = (y * pitch + x / 8, (y + h) * pitch + x / 8);
                    if xi >= b.len() { continue; }
                    let and = b[ai] & bit != 0;
                    let xor = b[xi] & bit != 0;
                    match (and, xor) {
                        (false, false) => { px(&mut norm, i, [0, 0, 0, 255]); any_n = true; }
                        (false, true) => { px(&mut norm, i, [255, 255, 255, 255]); any_n = true; }
                        (true, true) => { px(&mut inv, i, [255, 255, 255, 255]); any_i = true; }
                        (true, false) => {}
                    }
                } else {
                    let o = y * pitch + x * 4;
                    if o + 4 > b.len() { continue; }
                    let (c, a) = ([b[o], b[o + 1], b[o + 2]], b[o + 3]);
                    if si.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32 {
                        // mask 0: the colour as is; mask 0xFF: XOR with the screen (black = no change)
                        if a == 0 { px(&mut norm, i, [c[0], c[1], c[2], 255]); any_n = true; }
                        else if c != [0, 0, 0] { px(&mut inv, i, [255, 255, 255, 255]); any_i = true; }
                    } else if a > 0 {
                        px(&mut norm, i, [c[0], c[1], c[2], a]);
                        any_n = true;
                    }
                }
            }
        }
        let tex = |data: &[u8]| -> Result<ID3D11ShaderResourceView, String> {
            let td = D3D11_TEXTURE2D_DESC {
                Width: w as u32,
                Height: h as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr() as *const _, SysMemPitch: (w * 4) as u32, SysMemSlicePitch: 0 };
            let mut t = None;
            device.CreateTexture2D(&td, Some(&init), Some(&mut t)).map_err(e("cursor texture"))?;
            let t = t.ok_or("no cursor texture")?;
            let mut v = None;
            device.CreateShaderResourceView(&t, None, Some(&mut v)).map_err(e("cursor SRV"))?;
            v.ok_or_else(|| "no cursor srv".to_string())
        };
        self.norm = if any_n { Some(tex(&norm)?) } else { None };
        self.inv = if any_i { Some(tex(&inv)?) } else { None };
        self.w = w as u32;
        self.h = h as u32;
        Ok(())
    }

    /// Frame -> comp, pointer on top.
    fn draw(&self, ctx: &ID3D11DeviceContext, frame: &ID3D11Texture2D) {
        unsafe {
            ctx.CopyResource(&self.comp, frame);
            ctx.OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
            let vp = D3D11_VIEWPORT { TopLeftX: self.x as f32, TopLeftY: self.y as f32, Width: self.w as f32, Height: self.h as f32, MinDepth: 0.0, MaxDepth: 1.0 };
            ctx.RSSetViewports(Some(&[vp]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetSamplers(0, Some(&[Some(self.samp.clone())]));
            if let Some(n) = &self.norm {
                ctx.OMSetBlendState(&self.blend, None, 0xffff_ffff);
                ctx.PSSetShaderResources(0, Some(&[Some(n.clone())]));
                ctx.Draw(3, 0);
            }
            if let Some(i) = &self.inv {
                ctx.OMSetBlendState(&self.blend_inv, None, 0xffff_ffff);
                ctx.PSSetShaderResources(0, Some(&[Some(i.clone())]));
                ctx.Draw(3, 0);
            }
            ctx.OMSetBlendState(&self.opaque, None, 0xffff_ffff);
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
        }
    }
}

/// Where frames come from: the whole monitor (Desktop Duplication) or one window (Windows.Graphics.Capture).
enum Source {
    Dup { output: IDXGIOutput1, dup: Option<IDXGIOutputDuplication> },
    Win {
        _item: windows::Graphics::Capture::GraphicsCaptureItem,
        pool: windows::Graphics::Capture::Direct3D11CaptureFramePool,
        _session: windows::Graphics::Capture::GraphicsCaptureSession,
        d3d: windows::Graphics::DirectX::Direct3D11::IDirect3DDevice,
        size: windows::Graphics::SizeInt32,
    },
}

#[allow(dead_code)] // video device/enumerator kept alive for the processor
struct Capture {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    src: Source,
    /// adapter LUID, to pick the encoder on the same GPU
    luid: LUID,
    vendor: u32,
    hdr: Option<HdrConv>,
    /// current input size (window captures can change size)
    width: u32,
    height: u32,
    /// encoded (output) size - may be scaled down by the video processor
    out_w: u32,
    out_h: u32,
    bgra: ID3D11Texture2D,
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    venum: ID3D11VideoProcessorEnumerator,
    vproc: ID3D11VideoProcessor,
    in_view: ID3D11VideoProcessorInputView,
    nv12: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    /// mouse pointer drawn on top (whole-monitor capture only; window capture lets Windows draw it)
    cursor: Option<CursorDraw>,
    log: Log,
}

unsafe fn bgra_tex(device: &ID3D11Device, w: u32, h: u32) -> Result<ID3D11Texture2D, String> {
    let td = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut t = None;
    device.CreateTexture2D(&td, None, Some(&mut t)).map_err(e("CreateTexture2D bgra"))?;
    t.ok_or_else(|| "no bgra tex".to_string())
}

unsafe fn in_view(vdev: &ID3D11VideoDevice, venum: &ID3D11VideoProcessorEnumerator, tex: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView, String> {
    let ivd = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        FourCC: 0,
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
    };
    let mut v = None;
    vdev.CreateVideoProcessorInputView(tex, venum, &ivd, Some(&mut v)).map_err(e("VP input view"))?;
    v.ok_or_else(|| "no input view".to_string())
}

/// Start capturing one window with Windows.Graphics.Capture.
unsafe fn window_source(device: &ID3D11Device, hwnd: isize, hdr: bool, cursor: bool) -> Result<(Source, u32, u32), String> {
    use windows::Graphics::Capture::*;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
        return Err("window capture needs Windows 10 1903 or newer".into());
    }
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(e("capture interop"))?;
    let item: GraphicsCaptureItem = interop.CreateForWindow(HWND(hwnd as _)).map_err(e("CreateForWindow"))?;
    let dxgi: IDXGIDevice = device.cast().map_err(e("IDXGIDevice"))?;
    let d3d: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi).map_err(e("WinRT device"))?.cast().map_err(e("IDirect3DDevice"))?;
    let size = item.Size().map_err(e("item size"))?;
    let fmt = if hdr { DirectXPixelFormat::R16G16B16A16Float } else { DirectXPixelFormat::B8G8R8A8UIntNormalized };
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d, fmt, 2, size).map_err(e("frame pool"))?;
    let session = pool.CreateCaptureSession(&item).map_err(e("capture session"))?;
    let _ = session.SetIsCursorCaptureEnabled(cursor);
    let _ = session.SetIsBorderRequired(false); // Windows 11 only - ignored elsewhere
    session.StartCapture().map_err(e("StartCapture"))?;
    let (w, h) = ((size.Width.max(2) as u32) & !1, (size.Height.max(2) as u32) & !1);
    Ok((Source::Win { _item: item, pool, _session: session, d3d, size }, w, h))
}

impl Capture {
    fn new(pool: usize, o: &Opts, log: Log) -> Result<Self, String> {
        unsafe {
            let sel = pick_output(&o.monitor)?;
            let adesc = sel.adapter.GetDesc1().map_err(e("adapter desc"))?;
            let mut device = None;
            let mut ctx = None;
            D3D11CreateDevice(
                &sel.adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
            .map_err(e("D3D11CreateDevice"))?;
            let device = device.ok_or("no device")?;
            let ctx = ctx.ok_or("no context")?;
            // Media Foundation uses the device from its own threads.
            if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }

            // frame source: one window if asked (falls back to the monitor), else the monitor
            let mut src = None;
            let (mut width, mut height) = (sel.width, sel.height);
            if o.window != 0 {
                match window_source(&device, o.window, sel.hdr, o.cursor) {
                    Ok((s, w, h)) => {
                        log(&format!("gpu: recording the game window only ({w}x{h})"));
                        src = Some(s);
                        width = w;
                        height = h;
                    }
                    Err(err) => log(&format!("gpu: window capture failed ({err}) - recording the whole monitor")),
                }
            }
            let src = match src {
                Some(s) => s,
                None => Source::Dup { output: sel.output.clone(), dup: None },
            };
            // output size: the source size (even), scaled down to the chosen height
            let (out_w, out_h) = if o.target_h == 0 || o.target_h >= height {
                (width & !1, height & !1)
            } else {
                (((width as u64 * o.target_h as u64 / height as u64) as u32) & !1, o.target_h & !1)
            };

            let bgra = bgra_tex(&device, width, height)?;
            let hdr = if sel.hdr {
                let scale = sdr_scale(&sel.name);
                match HdrConv::new(&device, &bgra, width, height, scale) {
                    Ok(h) => {
                        log(&format!("gpu: HDR monitor - converting to SDR (SDR white {:.0} nits)", 80.0 / scale));
                        Some(h)
                    }
                    Err(err) => {
                        log(&format!("gpu: HDR conversion unavailable ({err}) - colours may look washed out"));
                        None
                    }
                }
            } else {
                None
            };

            let vdev: ID3D11VideoDevice = device.cast().map_err(e("ID3D11VideoDevice"))?;
            let vctx: ID3D11VideoContext = ctx.cast().map_err(e("ID3D11VideoContext"))?;
            let cdesc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: o.fps, Denominator: 1 },
                InputWidth: width,
                InputHeight: height,
                OutputFrameRate: DXGI_RATIONAL { Numerator: o.fps, Denominator: 1 },
                OutputWidth: out_w,
                OutputHeight: out_h,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let venum = vdev.CreateVideoProcessorEnumerator(&cdesc).map_err(e("VP enumerator"))?;
            let vproc = vdev.CreateVideoProcessor(&venum, 0).map_err(e("VideoProcessor"))?;
            // Colours: the desktop is full-range sRGB; players expect TV-range (16-235) BT.709 YUV.
            // Without this the processor wrote full-range values into a stream labelled TV-range -> dark, crushed clips.
            if let Ok(vc1) = vctx.cast::<ID3D11VideoContext1>() {
                vc1.VideoProcessorSetStreamColorSpace1(&vproc, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                vc1.VideoProcessorSetOutputColorSpace1(&vproc, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            } else {
                // bitfield: Usage:1 RGB_Range:1 YCbCr_Matrix:1 YCbCr_xvYCC:1 Nominal_Range:2
                let input = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 }; // full-range RGB
                let output = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) }; // BT.709, 16-235
                vctx.VideoProcessorSetStreamColorSpace(&vproc, 0, &input);
                vctx.VideoProcessorSetOutputColorSpace(&vproc, &output);
            }
            // black bars when a window's shape doesn't match the video
            let black = D3D11_VIDEO_COLOR { Anonymous: D3D11_VIDEO_COLOR_0 { YCbCr: D3D11_VIDEO_COLOR_YCbCrA { Y: 0.0625, Cb: 0.5, Cr: 0.5, A: 1.0 } } };
            vctx.VideoProcessorSetOutputBackgroundColor(&vproc, TRUE, &black);
            let in_v = in_view(&vdev, &venum, &bgra)?;

            let td = D3D11_TEXTURE2D_DESC {
                Width: out_w,
                Height: out_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let ovd = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut nv12 = Vec::new();
            for _ in 0..pool {
                let mut t = None;
                device.CreateTexture2D(&td, None, Some(&mut t)).map_err(e("CreateTexture2D nv12"))?;
                let t = t.ok_or("no nv12 tex")?;
                let mut v = None;
                vdev.CreateVideoProcessorOutputView(&t, &venum, &ovd, Some(&mut v)).map_err(e("VP output view"))?;
                nv12.push((t, v.ok_or("no output view")?));
            }

            let cursor = if o.cursor && matches!(src, Source::Dup { .. }) {
                match CursorDraw::new(&device, &vdev, &venum, width, height) {
                    Ok(c) => Some(c),
                    Err(err) => { log(&format!("gpu: mouse pointer can't be drawn ({err}) - recording without it")); None }
                }
            } else { None };
            let mut c = Capture {
                device, ctx, src, luid: adesc.AdapterLuid, vendor: adesc.VendorId, hdr, width, height, out_w, out_h, bgra,
                vdev, vctx, venum, vproc, in_view: in_v, nv12, cursor, log,
            };
            if let Source::Dup { .. } = c.src {
                c.reduplicate();
                if let Source::Dup { dup: None, .. } = c.src {
                    return Err("DuplicateOutput failed".into());
                }
            }
            c.fit();
            Ok(c)
        }
    }

    /// Aspect-correct placement of the input inside the output (black bars if needed).
    fn fit(&self) {
        unsafe {
            let (iw, ih, ow, oh) = (self.width as f64, self.height as f64, self.out_w as f64, self.out_h as f64);
            // whole monitor: always fill the picture. A game that switches the screen to a stretched mode
            // (Fortnite / CS2 "stretched res", e.g. 1440x1080 on a 1920x1080 screen) is shown stretched on the
            // monitor, so the clip looks the same (chat 21). One window: keep its shape (black bars)
            let s = (ow / iw).min(oh / ih);
            if matches!(self.src, Source::Dup { .. }) {
                let dst = RECT { left: 0, top: 0, right: self.out_w as i32, bottom: self.out_h as i32 };
                let src = RECT { left: 0, top: 0, right: self.width as i32, bottom: self.height as i32 };
                self.vctx.VideoProcessorSetStreamSourceRect(&self.vproc, 0, TRUE, Some(&src));
                self.vctx.VideoProcessorSetStreamDestRect(&self.vproc, 0, TRUE, Some(&dst));
                return;
            }
            let (w, h) = (iw * s, ih * s);
            let l = ((ow - w) / 2.0).round() as i32 & !1;
            let t = ((oh - h) / 2.0).round() as i32 & !1;
            let dst = RECT { left: l, top: t, right: (l + w.round() as i32).min(self.out_w as i32), bottom: (t + h.round() as i32).min(self.out_h as i32) };
            let src = RECT { left: 0, top: 0, right: self.width as i32, bottom: self.height as i32 };
            self.vctx.VideoProcessorSetStreamSourceRect(&self.vproc, 0, TRUE, Some(&src));
            self.vctx.VideoProcessorSetStreamDestRect(&self.vproc, 0, TRUE, Some(&dst));
        }
    }

    fn reduplicate(&mut self) {
        let hdr = self.hdr.is_some();
        if let Source::Dup { output, dup } = &mut self.src {
            *dup = unsafe {
                if hdr {
                    // HDR: ask for half-float frames (scRGB) and tone-map them ourselves
                    output
                        .cast::<IDXGIOutput5>()
                        .ok()
                        .and_then(|o5| o5.DuplicateOutput1(&self.device, 0, &[DXGI_FORMAT_R16G16B16A16_FLOAT]).ok())
                        .or_else(|| output.DuplicateOutput(&self.device).ok())
                } else {
                    output.DuplicateOutput(&self.device).ok()
                }
            };
        }
    }

    /// Whole-monitor capture: the screen mode changed size. New input texture; the pointer overlay is made again
    /// for the new size (it copies the frame), or dropped if that fails.
    fn screen_resized(&mut self, w: u32, h: u32) {
        let (ow, oh) = (self.width, self.height);
        if self.resize_input(w, h).is_err() {
            return;
        }
        if self.cursor.is_some() {
            self.cursor = CursorDraw::new(&self.device, &self.vdev, &self.venum, w, h).ok();
        }
        (self.log)(&format!("gpu: screen is now {w}x{h} (was {ow}x{oh}) - scaled to the recording size"));
    }

    /// The window changed size: new input texture + view (output size stays, the picture is letterboxed).
    fn resize_input(&mut self, w: u32, h: u32) -> Result<(), String> {
        unsafe {
            let bgra = bgra_tex(&self.device, w, h)?;
            let in_v = in_view(&self.vdev, &self.venum, &bgra)?;
            self.bgra = bgra;
            self.in_view = in_v;
            self.width = w;
            self.height = h;
            self.fit();
            Ok(())
        }
    }

    /// Grab the newest frame into `bgra` if there is one (otherwise keep the last one).
    fn grab(&mut self) {
        let hdr = self.hdr.is_some();
        if let Source::Dup { dup, .. } = &self.src {
            let Some(d) = dup.clone() else {
                self.reduplicate();
                return;
            };
            unsafe {
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut res: Option<IDXGIResource> = None;
                match d.AcquireNextFrame(0, &mut info, &mut res) {
                    Ok(()) => {
                        if info.LastPresentTime != 0 {
                            if let Some(tex) = res.and_then(|r| r.cast::<ID3D11Texture2D>().ok()) {
                                // the screen changed resolution (a game's own / stretched mode): new input size,
                                // scaled to the recording size (before: only the top-left part was copied)
                                let mut td = D3D11_TEXTURE2D_DESC::default();
                                tex.GetDesc(&mut td);
                                if !hdr && (td.Width != self.width || td.Height != self.height) && td.Width >= 320 && td.Height >= 240 {
                                    self.screen_resized(td.Width, td.Height);
                                }
                                self.put(&tex, hdr);
                            }
                        }
                        // the pointer moved / changed shape (only valid until ReleaseFrame)
                        if let Some(cur) = self.cursor.as_mut() {
                            cur.update(&self.device, &d, &info);
                        }
                        let _ = d.ReleaseFrame();
                    }
                    Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
                    Err(_) => {
                        // access lost (resolution change, UAC, fullscreen switch...) - recreate
                        if let Source::Dup { dup, .. } = &mut self.src {
                            *dup = None;
                        }
                        self.reduplicate();
                    }
                }
            }
            return;
        }
        // one window (Windows.Graphics.Capture)
        let (frame, resized) = match &mut self.src {
            Source::Win { pool, d3d, size, .. } => {
                let Ok(frame) = pool.TryGetNextFrame() else { return };
                let cs = frame.ContentSize().unwrap_or(*size);
                if cs.Width != size.Width || cs.Height != size.Height {
                    // window resized: recreate the pool at the new size; this frame is skipped
                    *size = cs;
                    let fmt = if hdr { windows::Graphics::DirectX::DirectXPixelFormat::R16G16B16A16Float } else { windows::Graphics::DirectX::DirectXPixelFormat::B8G8R8A8UIntNormalized };
                    let _ = pool.Recreate(&*d3d, fmt, 2, cs);
                    let _ = frame.Close();
                    (None, Some(cs))
                } else {
                    (Some(frame), None)
                }
            }
            _ => return,
        };
        if let Some(cs) = resized {
            let (w, h) = ((cs.Width.max(2) as u32) & !1, (cs.Height.max(2) as u32) & !1);
            if !hdr && (w != self.width || h != self.height) {
                let _ = self.resize_input(w, h);
            }
            return;
        }
        if let Some(frame) = frame {
            unsafe {
                let tex = frame
                    .Surface()
                    .ok()
                    .and_then(|s| s.cast::<windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess>().ok())
                    .and_then(|a| a.GetInterface::<ID3D11Texture2D>().ok());
                if let Some(tex) = tex {
                    self.put(&tex, hdr);
                }
            }
            let _ = frame.Close();
        }
    }

    /// Copy a captured texture into `bgra` (through the HDR converter when needed). Sizes may differ (window).
    unsafe fn put(&self, tex: &ID3D11Texture2D, hdr: bool) {
        let mut d = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut d);
        let target: &ID3D11Texture2D = match (&self.hdr, hdr) {
            (Some(h), true) if d.Format == DXGI_FORMAT_R16G16B16A16_FLOAT => &h.fp16,
            _ if d.Format == DXGI_FORMAT_R16G16B16A16_FLOAT => return, // no converter for this frame
            _ => &self.bgra,
        };
        if d.Width == self.width && d.Height == self.height {
            self.ctx.CopyResource(target, tex);
        } else {
            let b = D3D11_BOX { left: 0, top: 0, front: 0, right: d.Width.min(self.width), bottom: d.Height.min(self.height), back: 1 };
            self.ctx.CopySubresourceRegion(target, 0, 0, 0, 0, tex, 0, Some(&b));
        }
        if let (Some(h), true) = (&self.hdr, d.Format == DXGI_FORMAT_R16G16B16A16_FLOAT) {
            h.draw(&self.ctx);
        }
    }

    /// Convert current frame to NV12 into pool slot `i`.
    fn convert(&self, i: usize) -> Result<&ID3D11Texture2D, String> {
        unsafe {
            let (tex, view) = &self.nv12[i];
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: TRUE,
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: ManuallyDrop::new(Some(match self.cursor.as_ref().filter(|c| c.showing()) {
                    // pointer on screen: frame copy + pointer on top (the clean frame stays for the next tick)
                    Some(c) => { c.draw(&self.ctx, &self.bgra); c.view.clone() }
                    None => self.in_view.clone(),
                })),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let streams = [stream];
            let r = self.vctx.VideoProcessorBlt(&self.vproc, view, 0, &streams);
            let [mut s] = streams;
            ManuallyDrop::drop(&mut s.pInputSurface);
            r.map_err(e("VideoProcessorBlt"))?;
            Ok(tex)
        }
    }
}

// ---- Dark scenes (12 Nov 2026) ----
// Smart bitrate = peak-limited VBR with an average below the ceiling. The AMD encoder keeps that average over a long
// window: busy scenes go above it and calm scenes pay the debt back. Night scenes look "calm" to it, so they fell to
// ~1 Mbps at QP ~22 (FiveM night drive: smeared road, blocky sky) while dark low-contrast detail needs a FINER
// quantizer than bright detail. Fix: ~4x a second, read a few thin stripes of the frame back from the GPU; while most
// of the picture is dark AND the encoder spends less than half its average, lower the encoder's maximum QP one step at
// a time (24 -> 16). Busy dark scenes (city lights) already get their bits and are left alone; bright scenes untouched.
// Offline with the same AMD encoder (day footage, then a dark scene): dark parts +1.3-1.9 dB, ~2-4 instead of ~1-2 Mbps.

const PROBE_STRIPES: u32 = 16;
const PROBE_ROWS: u32 = 8;
/// how often (in frames) the brightness is sampled and the cap revised: ~4x a second at 60 fps
const DARK_EVERY: u64 = 15;

/// Copies 16 thin stripes (8 rows each) of the NV12 frame into a small CPU-readable texture. It is read one sample
/// later (DARK_EVERY frames), so the GPU has long finished and nothing waits.
struct DarkProbe {
    tex: ID3D11Texture2D,
    w: u32,
    h: u32,
    pending: bool,
}

impl DarkProbe {
    unsafe fn new(device: &ID3D11Device, w: u32, h: u32) -> Option<Self> {
        let (w, h) = (w & !1, h & !1);
        if w < 64 || h < PROBE_STRIPES * PROBE_ROWS * 2 {
            return None;
        }
        let td = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: PROBE_STRIPES * PROBE_ROWS,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut t = None;
        device.CreateTexture2D(&td, None, Some(&mut t)).ok()?;
        Some(DarkProbe { tex: t?, w, h, pending: false })
    }

    unsafe fn sample(&mut self, ctx: &ID3D11DeviceContext, frame: &ID3D11Texture2D) {
        for s in 0..PROBE_STRIPES {
            let top = ((2 * s + 1) * self.h / (2 * PROBE_STRIPES)) & !1;
            let b = D3D11_BOX { left: 0, top, front: 0, right: self.w, bottom: top + PROBE_ROWS, back: 1 };
            ctx.CopySubresourceRegion(&self.tex, 0, 0, s * PROBE_ROWS, 0, frame, 0, Some(&b));
        }
        self.pending = true;
    }

    /// Dark share (0-100) of the previous sample; None if there is none or it can't be read.
    unsafe fn read(&mut self, ctx: &ID3D11DeviceContext) -> Option<u32> {
        if !std::mem::take(&mut self.pending) {
            return None;
        }
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&self.tex, 0, D3D11_MAP_READ, 0, Some(&mut m)).ok()?;
        let rows: Vec<&[u8]> = (0..(PROBE_STRIPES * PROBE_ROWS) as usize)
            .map(|y| std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), self.w as usize))
            .collect();
        let r = dark_share(&rows);
        ctx.Unmap(&self.tex, 0);
        r
    }
}

/// Share (0-100) of 64-pixel cells darker than luma 48 (TV range, ~15 % grey) in PROBE_ROWS-row stripes.
/// None when it reads as all zero: TV-range luma is never below 16, so the copy didn't happen.
fn dark_share(rows: &[&[u8]]) -> Option<u32> {
    let cells_x = rows.first()?.len() / 64;
    let (mut dark, mut cells, mut sum, mut count) = (0u32, 0u32, 0u64, 0u64);
    for stripe in rows.chunks(PROBE_ROWS as usize) {
        for cx in 0..cells_x {
            let (mut s, mut n) = (0u64, 0u64);
            for row in stripe {
                for x in (cx * 64..cx * 64 + 64).step_by(4) {
                    s += row[x] as u64;
                    n += 1;
                }
            }
            dark += (s < 48 * n) as u32;
            cells += 1;
            sum += s;
            count += n;
        }
    }
    if cells == 0 || sum < 8 * count {
        return None;
    }
    Some(dark * 100 / cells)
}

/// The encoder's maximum QP for dark scenes; 51 = no cap.
struct DarkCap {
    cap: u32,
    floor: u32,
    /// lowest cap allowed in this dark stretch: raised above a cap that pushed the output to the ceiling, so a busy
    /// night scene doesn't pump (cap on -> over the ceiling -> off -> starved -> on ...; seen at a 6 Mbps max)
    low: u32,
    /// the average bitrate the encoder aims for and its ceiling (bits/s)
    mean: u64,
    peak: u64,
}

impl DarkCap {
    fn new(mean: u32, peak: u32, quality: u32) -> Self {
        let floor = if quality <= 72 { 16 } else if quality <= 82 { 15 } else { 14 };
        DarkCap { cap: 51, floor, low: floor, mean: mean as u64, peak: peak as u64 }
    }

    /// Revise the cap from the dark share and the encoder's output over the last second; Some(new cap) on a change.
    fn step(&mut self, dark_pct: Option<u32>, rate_bps: u64) -> Option<u32> {
        let old = self.cap;
        self.cap = if dark_pct.map_or(true, |p| p < 60) {
            self.low = self.floor;
            51
        } else if rate_bps * 5 > self.peak * 4 {
            // close to the ceiling: busy enough without help - and never this low again in this dark stretch
            if self.cap < 51 { self.low = self.low.max(self.cap + 2); }
            51
        } else if rate_bps * 2 < self.mean {
            if self.low >= 28 { 51 } else if self.cap > 24 { 24.max(self.low) } else { (self.cap - 1).max(self.low) }
        } else if rate_bps > self.mean {
            if self.cap >= 28 { 51 } else { self.cap + 2 }
        } else {
            self.cap
        };
        (self.cap != old).then_some(self.cap)
    }
}

#[cfg(test)]
mod dark_tests {
    use super::*;

    /// A busy night scene whose output grows as the cap goes down: the cap must settle, not pump on and off.
    #[test]
    fn busy_dark_scene_settles() {
        let (mean, peak) = (3_600_000u32, 6_000_000u32);
        let mut d = DarkCap::new(mean, peak, 72);
        // output of this scene at a cap: 1 Mbps uncapped, +0.6 Mbps per step below 24
        let rate = |cap: u32| if cap >= 24 { 1_000_000u64 } else { 1_000_000 + (24 - cap as u64) * 600_000 };
        let mut offs = 0;
        for _ in 0..200 {
            let r = rate(d.cap);
            if let Some(51) = d.step(Some(95), r) { offs += 1; }
        }
        assert!(offs <= 2, "cap switched off {offs} times");
        assert!(d.cap < 24 && rate(d.cap) * 5 <= peak as u64 * 4, "settled at {} ({} bps)", d.cap, rate(d.cap));
        // the scene turns bright: no cap, and the next dark stretch starts from the normal floor again
        assert_eq!(d.step(Some(10), 1_000_000), Some(51));
        assert_eq!(d.low, d.floor);
    }

    /// A starved dark scene that stays cheap goes down to the floor and stays there.
    #[test]
    fn starved_dark_scene_reaches_floor() {
        let mut d = DarkCap::new(12_000_000, 20_000_000, 72);
        for _ in 0..40 { d.step(Some(95), 2_000_000); }
        assert_eq!(d.cap, 16);
    }

    #[test]
    fn dark_share_counts_dark_cells_and_rejects_zeros() {
        let dark = vec![20u8; 128];
        let bright = vec![120u8; 128];
        let rows: Vec<&[u8]> = (0..PROBE_ROWS).map(|_| &dark[..]).chain((0..PROBE_ROWS).map(|_| &bright[..])).collect();
        assert_eq!(dark_share(&rows), Some(50));
        let zeros = vec![0u8; 128];
        let z: Vec<&[u8]> = (0..PROBE_ROWS).map(|_| &zeros[..]).collect();
        assert_eq!(dark_share(&z), None);
    }
}

/// Encoder output of the last 4 x DARK_EVERY frames, as bits per second.
struct RateWin {
    buckets: [u64; 4],
    i: usize,
}

impl RateWin {
    fn add(&mut self, bytes: usize) {
        self.buckets[self.i] += bytes as u64;
    }
    /// close the current bucket; returns the rate over the window
    fn roll(&mut self, fps: u32) -> u64 {
        let total: u64 = self.buckets.iter().sum();
        self.i = (self.i + 1) % self.buckets.len();
        self.buckets[self.i] = 0;
        total * 8 * fps as u64 / (self.buckets.len() as u64 * DARK_EVERY)
    }
}

/// Where the encoder's NV12 frames come from: the screen (Capture), or decoded test frames (encode_test).
trait FrameSrc {
    fn device(&self) -> &ID3D11Device;
    fn ctx(&self) -> &ID3D11DeviceContext;
    fn luid(&self) -> LUID;
    fn vendor(&self) -> u32;
    fn out_w(&self) -> u32;
    fn out_h(&self) -> u32;
    /// take the newest frame
    fn grab(&mut self);
    /// that frame as NV12 in pool slot `i`
    fn convert(&self, i: usize) -> Result<&ID3D11Texture2D, String>;
}

impl FrameSrc for Capture {
    fn device(&self) -> &ID3D11Device { &self.device }
    fn ctx(&self) -> &ID3D11DeviceContext { &self.ctx }
    fn luid(&self) -> LUID { self.luid }
    fn vendor(&self) -> u32 { self.vendor }
    fn out_w(&self) -> u32 { self.out_w }
    fn out_h(&self) -> u32 { self.out_h }
    fn grab(&mut self) { Capture::grab(self) }
    fn convert(&self, i: usize) -> Result<&ID3D11Texture2D, String> { Capture::convert(self, i) }
}

/// Test hook: the encoder "refuses" every max QP below 51 (the dark-scene boost must switch itself off)
#[doc(hidden)]
pub static TEST_REFUSE_MAX_QP: AtomicBool = AtomicBool::new(false);

struct Encoder {
    mft: IMFTransform,
    api: Option<ICodecAPI>,
    events: IMFMediaEventGenerator,
    _mgr: IMFDXGIDeviceManager,
    /// the average bitrate it aims for (bits/s)
    mean_bps: u32,
    /// the encoder takes a maximum QP (CODECAPI_AVEncVideoMaxQP) - the dark-scene cap needs it
    max_qp: bool,
}

unsafe fn set_codec(api: &ICodecAPI, key: &GUID, v: u32) {
    let _ = api.SetValue(key, &VARIANT::from(v));
}

/// Set a rate control mode and read it back: some encoders accept the call but keep their old mode.
unsafe fn try_rate_mode(api: &ICodecAPI, m: eAVEncCommonRateControlMode) -> bool {
    api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &VARIANT::from(m.0 as u32)).is_ok()
        && api.GetValue(&CODECAPI_AVEncCommonRateControlMode).ok().and_then(|v| u32::try_from(&v).ok()) == Some(m.0 as u32)
}

impl Encoder {
    fn new(cap: &impl FrameSrc, fps: u32, bitrate: u32, gop: u32, codec: &str, smart: bool, quality: u32, log: Log) -> Result<Self, String> {
        unsafe {
            let (subtype, cname) = match codec {
                "hevc" => (MFVideoFormat_HEVC, "HEVC"),
                "av1" => (MFVideoFormat_AV1, "AV1"),
                _ => (MFVideoFormat_H264, "H.264"),
            };
            let out_info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype };
            let mut acts: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            // 1) the encoder on the same GPU as the capture (needed with two GPUs, e.g. NVIDIA + Intel/AMD)
            let luid = [cap.luid().LowPart.to_le_bytes(), (cap.luid().HighPart as u32).to_le_bytes()].concat();
            let mut attrs = None;
            if MFCreateAttributes(&mut attrs, 1).is_ok() {
                if let Some(a) = &attrs {
                    if a.SetBlob(&MFT_ENUM_ADAPTER_LUID, &luid).is_ok() {
                        let _ = MFTEnum2(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                            None, Some(&out_info), a, &mut acts, &mut count);
                    }
                }
            }
            // 2) any hardware encoder (older Windows)
            if count == 0 || acts.is_null() {
                count = 0;
                MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, None, Some(&out_info), &mut acts, &mut count)
                    .map_err(e("MFTEnumEx"))?;
            }
            if count == 0 || acts.is_null() {
                return Err(format!("No hardware {cname} encoder (MFT) found on this GPU"));
            }
            let list = std::slice::from_raw_parts_mut(acts, count as usize);
            // prefer the capture GPU's vendor (VEN_1002 AMD, VEN_10DE NVIDIA, VEN_8086 Intel), else the first
            let want = format!("VEN_{:04X}", cap.vendor());
            let vendor_of = |a: &IMFActivate| {
                let mut buf = [0u16; 64];
                let mut n = 0u32;
                let _ = a.GetString(&MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, &mut buf, Some(&mut n));
                String::from_utf16_lossy(&buf[..n as usize])
            };
            let idx = list.iter().position(|a| a.as_ref().map_or(false, |a| vendor_of(a).eq_ignore_ascii_case(&want))).unwrap_or(0);
            let act = list[idx].clone().ok_or("null activate")?;
            let mut name_buf = [0u16; 256];
            let mut name_len = 0u32;
            let _ = act.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut name_buf, Some(&mut name_len));
            log(&format!("gpu: {cname} encoder '{}' ({})", String::from_utf16_lossy(&name_buf[..name_len as usize]), vendor_of(&act)));
            let mft: IMFTransform = act.ActivateObject().map_err(e("ActivateObject"))?;
            for a in list.iter_mut() {
                drop(a.take());
            }
            CoTaskMemFree(Some(acts as *const _));

            let attrs = mft.GetAttributes().map_err(e("GetAttributes"))?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).map_err(e("ASYNC_UNLOCK"))?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);

            let mut token = 0u32;
            let mut mgr = None;
            MFCreateDXGIDeviceManager(&mut token, &mut mgr).map_err(e("DXGIDeviceManager"))?;
            let mgr = mgr.ok_or("no device manager")?;
            mgr.ResetDevice(cap.device(), token).map_err(e("ResetDevice"))?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr.as_raw() as usize)
                .map_err(e("SET_D3D_MANAGER"))?;

            let api = mft.cast::<ICodecAPI>().ok();
            // average the encoder aims for: the full bitrate for CBR, lower for smart bitrate (some encoders, e.g. AMD,
            // take their target from the output type's MF_MT_AVG_BITRATE rather than from ICodecAPI)
            let mut avg_target = bitrate;
            if let Some(api) = &api {
                // Smart bitrate: peak-limited VBR. The bitrate setting is a hard ceiling (never bigger than the old CBR clips);
                // the average target follows the quality level (72 -> 60%, 82 -> 75%, 90 -> 90%), and calm moments fall far
                // below it. (MF "Quality" mode was tried in chat 15: AMD ignored MaxBitRate there - 88 Mbps at a 12 Mbps cap.)
                let pct = if quality <= 72 { 60 } else if quality <= 82 { 75 } else { 90 };
                let mean = bitrate / 100 * pct;
                let vbr = smart && try_rate_mode(api, eAVEncCommonRateControlMode_PeakConstrainedVBR);
                if vbr { avg_target = mean; }
                let mode = if vbr {
                    set_codec(&api, &CODECAPI_AVEncCommonMeanBitRate, mean);
                    set_codec(&api, &CODECAPI_AVEncCommonMaxBitRate, bitrate);
                    format!("peak-limited VBR, average {pct}%")
                } else {
                    set_codec(&api, &CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32);
                    set_codec(&api, &CODECAPI_AVEncCommonMeanBitRate, bitrate);
                    if smart { "CBR (smart bitrate not supported by this encoder)".to_string() } else { "CBR".to_string() }
                };
                log(&format!("gpu: rate control {mode}, max {} Mbps", bitrate / 1_000_000));
                set_codec(&api, &CODECAPI_AVEncMPVGOPSize, gop);
                set_codec(&api, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
            }

            let size = ((cap.out_w() as u64) << 32) | cap.out_h() as u64;
            let rate = ((fps as u64) << 32) | 1;
            let ot = MFCreateMediaType().map_err(e("MFCreateMediaType"))?;
            ot.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("out major"))?;
            ot.SetGUID(&MF_MT_SUBTYPE, &subtype).map_err(e("out subtype"))?;
            ot.SetUINT32(&MF_MT_AVG_BITRATE, avg_target).map_err(e("out bitrate"))?;
            ot.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(e("out size"))?;
            ot.SetUINT64(&MF_MT_FRAME_RATE, rate).map_err(e("out rate"))?;
            ot.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1).map_err(e("out par"))?;
            ot.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(e("out il"))?;
            let _ = ot.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
            let _ = ot.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32);
            let _ = ot.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32);
            let _ = ot.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32);
            match codec {
                "hevc" => { let _ = ot.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH265VProfile_Main_420_8.0 as u32); }
                "av1" => {}
                _ => ot.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32).map_err(e("out profile"))?,
            }
            mft.SetOutputType(0, &ot, 0).map_err(e("SetOutputType"))?;
            // setting the output type can reset rate control on some drivers: apply it again
            if let (Some(api), true) = (&api, avg_target != bitrate) {
                if try_rate_mode(api, eAVEncCommonRateControlMode_PeakConstrainedVBR) {
                    set_codec(&api, &CODECAPI_AVEncCommonMeanBitRate, avg_target);
                    set_codec(&api, &CODECAPI_AVEncCommonMaxBitRate, bitrate);
                }
                let got = api.GetValue(&CODECAPI_AVEncCommonMeanBitRate).ok().and_then(|v| u32::try_from(&v).ok()).unwrap_or(0);
                log(&format!("gpu: average bitrate after setup {:.1} Mbps", got as f64 / 1e6));
            }

            let it = MFCreateMediaType().map_err(e("MFCreateMediaType"))?;
            it.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("in major"))?;
            it.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(e("in subtype"))?;
            it.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(e("in size"))?;
            it.SetUINT64(&MF_MT_FRAME_RATE, rate).map_err(e("in rate"))?;
            it.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1).map_err(e("in par"))?;
            it.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(e("in il"))?;
            let _ = it.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
            let _ = it.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32);
            let _ = it.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32);
            let _ = it.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32);
            mft.SetInputType(0, &it, 0).map_err(e("SetInputType"))?;

            let events: IMFMediaEventGenerator = mft.cast().map_err(e("event generator"))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(e("BEGIN_STREAMING"))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(e("START_OF_STREAM"))?;
            let max_qp = api.as_ref().map_or(false, |a| a.IsSupported(&CODECAPI_AVEncVideoMaxQP).is_ok());
            Ok(Encoder { mft, api, events, _mgr: mgr, mean_bps: avg_target, max_qp })
        }
    }

    /// Change the maximum QP while encoding (dark scenes); false if the encoder refused it.
    fn set_max_qp(&self, qp: u32) -> bool {
        if qp != 51 && TEST_REFUSE_MAX_QP.load(Ordering::Relaxed) {
            return false;
        }
        self.api.as_ref().map_or(false, |a| unsafe { a.SetValue(&CODECAPI_AVEncVideoMaxQP, &VARIANT::from(qp)).is_ok() })
    }

    /// Ask for an IDR frame (segments can only be cut on keyframes).
    fn force_key(&self) {
        if let Some(api) = &self.api {
            unsafe { set_codec(api, &CODECAPI_AVEncVideoForceKeyFrame, 1) };
        }
    }

    fn feed(&self, tex: &ID3D11Texture2D, time: i64, dur: i64) -> Result<(), String> {
        unsafe {
            let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, tex, 0, false)
                .map_err(e("DXGISurfaceBuffer"))?;
            if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
                if let Ok(len) = b2.GetContiguousLength() {
                    let _ = buf.SetCurrentLength(len);
                }
            }
            let sample = MFCreateSample().map_err(e("MFCreateSample"))?;
            sample.AddBuffer(&buf).map_err(e("AddBuffer"))?;
            sample.SetSampleTime(time).map_err(e("SetSampleTime"))?;
            sample.SetSampleDuration(dur).map_err(e("SetSampleDuration"))?;
            self.mft.ProcessInput(0, &sample, 0).map_err(e("ProcessInput"))
        }
    }

    /// Pull one encoded frame (Annex B H.264 / HEVC, or AV1 OBUs).
    fn drain(&self, out: &mut Vec<u8>) -> Result<(), String> {
        unsafe {
            let mut bufs = [MFT_OUTPUT_DATA_BUFFER::default()];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut bufs, &mut status);
            let sample = ManuallyDrop::take(&mut bufs[0].pSample);
            let _events = ManuallyDrop::take(&mut bufs[0].pEvents);
            if let Err(err) = r {
                if err.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                    let t = self.mft.GetOutputAvailableType(0, 0).map_err(e("GetOutputAvailableType"))?;
                    self.mft.SetOutputType(0, &t, 0).map_err(e("SetOutputType (change)"))?;
                    return Ok(());
                }
                return Err(format!("ProcessOutput: {err}"));
            }
            let Some(sample) = sample else { return Ok(()) };
            let buf = sample.ConvertToContiguousBuffer().map_err(e("ContiguousBuffer"))?;
            let mut p = std::ptr::null_mut();
            let mut len = 0u32;
            buf.Lock(&mut p, None, Some(&mut len)).map_err(e("Lock"))?;
            out.extend_from_slice(std::slice::from_raw_parts(p, len as usize));
            let _ = buf.Unlock();
            Ok(())
        }
    }
}

fn write_all(pipe: HANDLE, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let mut n = 0u32;
        if unsafe { WriteFile(pipe, Some(data), Some(&mut n), None) }.is_err() || n == 0 {
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

/// Runs until `stop` is set or ffmpeg goes away. Call on its own thread.
pub fn run(pipe: Pipe, stop: Arc<AtomicBool>, o: Opts, log: Log) {
    let pipe = HANDLE(pipe.0 as _);
    let res = (|| -> Result<(), String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(e("MFStartup"))?;
        }
        let pool = 8;
        let mut cap = Capture::new(pool, &o, log)?;
        log(&format!("gpu: capturing {}x{} -> {}x{} @{}fps (zero-copy)", cap.width, cap.height, cap.out_w, cap.out_h, o.fps));
        encode(&mut cap, pool, pipe, &stop, &o, log)
    })();
    if let Err(err) = res {
        log(&format!("gpu: failed: {err}"));
    }
    unsafe {
        let _ = CloseHandle(pipe);
    }
}

/// The encoder loop: frames from `cap` -> hardware encoder -> `pipe`, paced to real time, until `stop` or the reader
/// (ffmpeg) goes away.
fn encode(cap: &mut impl FrameSrc, pool: usize, pipe: HANDLE, stop: &AtomicBool, o: &Opts, log: Log) -> Result<(), String> {
    let fps = o.fps;
    let gop = o.gop;
    let enc = Encoder::new(cap, fps, o.bitrate_mbps * 1_000_000, gop, &o.codec, o.smart_bitrate, o.smart_quality, log)?;
    // dark scenes get a lower max QP while the encoder starves them (see DarkProbe)
    let mut probe = if enc.max_qp { unsafe { DarkProbe::new(cap.device(), cap.out_w(), cap.out_h()) } } else { None };
    if probe.is_none() {
        log(if enc.max_qp { "gpu: dark-scene boost off (no frame probe)" } else { "gpu: dark-scene boost off (encoder has no max QP)" });
    }
    let mut dark = DarkCap::new(enc.mean_bps, o.bitrate_mbps * 1_000_000, o.smart_quality);
    let mut win = RateWin { buckets: [0; 4], i: 0 };
    let (mut dark_logs, mut unread) = (0u32, 0u32);
    // what the dark-scene checks cost (CPU time on this thread), logged when recording stops
    let (mut checks, mut check_time) = (0u32, Duration::ZERO);

    unsafe {
        if let Err(err) = ConnectNamedPipe(pipe, None) {
            if err.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                return Err(format!("ConnectNamedPipe: {err}"));
            }
        }
    }
    log("gpu: ffmpeg connected, encoding");

    let frame = Duration::from_secs_f64(1.0 / fps as f64);
    let dur = 10_000_000i64 / fps as i64;
    let start = Instant::now();
    let mut n: u64 = 0;
    let mut out = Vec::with_capacity(1 << 20);
    while !stop.load(Ordering::Relaxed) {
        let ev = unsafe { enc.events.GetEvent(MF_EVENT_FLAG_NONE) }.map_err(e("GetEvent"))?;
        let kind = unsafe { ev.GetType() }.map_err(e("GetType"))?;
        if kind == METransformNeedInput.0 as u32 {
            // pace to real time
            let due = start + frame * n as u32;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            } else if now - due > frame * 2 {
                LATE.fetch_add(1, Ordering::Relaxed);
            }
            FRAMES.fetch_add(1, Ordering::Relaxed);
            cap.grab();
            let tex = cap.convert((n as usize) % pool)?;
            if n % DARK_EVERY == 0 {
                let mut off = None;
                if let Some(p) = probe.as_mut() {
                    let t0 = Instant::now();
                    let share = unsafe { p.read(cap.ctx()) };
                    unsafe { p.sample(cap.ctx(), tex) };
                    let rate = win.roll(fps);
                    // a driver that can't copy parts of an NV12 frame reads as all zero: stop after 2 s of that
                    unread = if share.is_some() || n == 0 { 0 } else { unread + 1 };
                    if unread >= 8 {
                        off = Some("frames can't be read back".to_string());
                    }
                    if let Some(q) = dark.step(share, rate) {
                        let ok = enc.set_max_qp(q);
                        if !ok {
                            off = Some(format!("encoder refused max QP {q}"));
                        } else if dark_logs < 20 && (q == 24 || q == 51) {
                            // the start and end of a dark stretch (the steps in between would fill the log)
                            dark_logs += 1;
                            log(&format!("gpu: dark scene {}%, {:.1} Mbps -> max QP {q}", share.unwrap_or(0), rate as f64 / 1e6));
                        }
                    }
                    checks += 1;
                    check_time += t0.elapsed();
                }
                if let Some(why) = off {
                    log(&format!("gpu: dark-scene boost off ({why})"));
                    let _ = enc.set_max_qp(51);
                    probe = None;
                }
            }
            if n % gop as u64 == 0 {
                enc.force_key();
            }
            enc.feed(tex, n as i64 * dur, dur)?;
            n += 1;
        } else if kind == METransformHaveOutput.0 as u32 {
            out.clear();
            enc.drain(&mut out)?;
            win.add(out.len());
            if !out.is_empty() && !write_all(pipe, &out) {
                break; // ffmpeg exited
            }
        }
    }
    if checks > 0 {
        log(&format!("gpu: {n} frames; dark-scene checks: {checks}, {:.0} us each (CPU)",
            check_time.as_secs_f64() * 1e6 / checks as f64));
    }
    Ok(())
}

/// Test hook: raw NV12 frames (w x h, read from `frames`) uploaded to GPU textures, standing in for the screen.
struct TestFrames {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    luid: LUID,
    vendor: u32,
    w: u32,
    h: u32,
    pool: Vec<ID3D11Texture2D>,
    staging: ID3D11Texture2D,
    frames: Box<dyn std::io::Read + Send>,
    buf: Vec<u8>,
    /// set when the frames run out
    done: Arc<AtomicBool>,
}

impl FrameSrc for TestFrames {
    fn device(&self) -> &ID3D11Device { &self.device }
    fn ctx(&self) -> &ID3D11DeviceContext { &self.ctx }
    fn luid(&self) -> LUID { self.luid }
    fn vendor(&self) -> u32 { self.vendor }
    fn out_w(&self) -> u32 { self.w }
    fn out_h(&self) -> u32 { self.h }
    fn grab(&mut self) {
        if self.frames.read_exact(&mut self.buf).is_err() {
            self.done.store(true, Ordering::Relaxed);
        }
    }
    fn convert(&self, i: usize) -> Result<&ID3D11Texture2D, String> {
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&self.staging, 0, D3D11_MAP_WRITE, 0, Some(&mut m)).map_err(e("Map staging"))?;
            let w = self.w as usize;
            for y in 0..self.h as usize * 3 / 2 {
                std::ptr::copy_nonoverlapping(self.buf.as_ptr().add(y * w), (m.pData as *mut u8).add(y * m.RowPitch as usize), w);
            }
            self.ctx.Unmap(&self.staging, 0);
            let t = &self.pool[i % self.pool.len()];
            self.ctx.CopyResource(t, &self.staging);
            Ok(t)
        }
    }
}

/// Test hook (headless end-to-end test, echo-core example `gpucap_test`): the recorder's real encoder loop (`encode`)
/// fed with raw NV12 frames (w x h) from `frames` instead of the screen, on the GPU with the most video memory.
/// Writes the encoded stream into `pipe` (the caller reads it); returns when the frames run out.
#[doc(hidden)]
pub fn encode_test(frames: Box<dyn std::io::Read + Send>, w: u32, h: u32, pipe: Pipe, o: Opts, log: Log) -> Result<(), String> {
    let pipe = HANDLE(pipe.0 as _);
    let r = (|| -> Result<(), String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(e("MFStartup"))?;
            let fac: IDXGIFactory1 = CreateDXGIFactory1().map_err(e("DXGIFactory"))?;
            let mut best: Option<(IDXGIAdapter1, DXGI_ADAPTER_DESC1)> = None;
            let mut i = 0;
            while let Ok(a) = fac.EnumAdapters1(i) {
                let d = a.GetDesc1().map_err(e("GetDesc1"))?;
                if best.as_ref().map_or(true, |(_, b)| d.DedicatedVideoMemory > b.DedicatedVideoMemory) {
                    best = Some((a, d));
                }
                i += 1;
            }
            let (adapter, desc) = best.ok_or("no GPU")?;
            let (mut dev, mut ctx) = (None, None);
            D3D11CreateDevice(&adapter, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION,
                Some(&mut dev), None, Some(&mut ctx)).map_err(e("D3D11CreateDevice"))?;
            let (device, ctx): (ID3D11Device, ID3D11DeviceContext) = (dev.ok_or("no device")?, ctx.ok_or("no context")?);
            if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }
            let td = D3D11_TEXTURE2D_DESC {
                Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32, CPUAccessFlags: 0, MiscFlags: 0,
            };
            let mut pool = Vec::new();
            for _ in 0..8 {
                let mut t = None;
                device.CreateTexture2D(&td, None, Some(&mut t)).map_err(e("CreateTexture2D"))?;
                pool.push(t.ok_or("no texture")?);
            }
            let sd = D3D11_TEXTURE2D_DESC { Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32, ..td };
            let mut st = None;
            device.CreateTexture2D(&sd, None, Some(&mut st)).map_err(e("CreateTexture2D staging"))?;
            let done = Arc::new(AtomicBool::new(false));
            let mut src = TestFrames {
                device, ctx, luid: desc.AdapterLuid, vendor: desc.VendorId, w, h, pool, staging: st.ok_or("no staging")?,
                frames, buf: vec![0; (w * h * 3 / 2) as usize], done: done.clone(),
            };
            log(&format!("gpu: TEST frames {w}x{h} @{}fps", o.fps));
            encode(&mut src, 8, pipe, &done, &o, log)
        }
    })();
    unsafe {
        let _ = CloseHandle(pipe);
    }
    r
}
