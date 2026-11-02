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
    /// window to record (HWND as isize); 0 = whole monitor
    pub window: isize,
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
unsafe fn window_source(device: &ID3D11Device, hwnd: isize, hdr: bool) -> Result<(Source, u32, u32), String> {
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
    let _ = session.SetIsCursorCaptureEnabled(false);
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
                match window_source(&device, o.window, sel.hdr) {
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

            let mut c = Capture {
                device, ctx, src, luid: adesc.AdapterLuid, vendor: adesc.VendorId, hdr, width, height, out_w, out_h, bgra,
                vdev, vctx, venum, vproc, in_view: in_v, nv12,
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
            let s = (ow / iw).min(oh / ih);
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
                                self.put(&tex, hdr);
                            }
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
                pInputSurface: ManuallyDrop::new(Some(self.in_view.clone())),
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

struct Encoder {
    mft: IMFTransform,
    api: Option<ICodecAPI>,
    events: IMFMediaEventGenerator,
    _mgr: IMFDXGIDeviceManager,
}

unsafe fn set_codec(api: &ICodecAPI, key: &GUID, v: u32) {
    let _ = api.SetValue(key, &VARIANT::from(v));
}

impl Encoder {
    fn new(cap: &Capture, fps: u32, bitrate: u32, gop: u32, codec: &str, log: Log) -> Result<Self, String> {
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
            let luid = [cap.luid.LowPart.to_le_bytes(), (cap.luid.HighPart as u32).to_le_bytes()].concat();
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
            let want = format!("VEN_{:04X}", cap.vendor);
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
            mgr.ResetDevice(&cap.device, token).map_err(e("ResetDevice"))?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr.as_raw() as usize)
                .map_err(e("SET_D3D_MANAGER"))?;

            let api = mft.cast::<ICodecAPI>().ok();
            if let Some(api) = &api {
                set_codec(&api, &CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32);
                set_codec(&api, &CODECAPI_AVEncCommonMeanBitRate, bitrate);
                set_codec(&api, &CODECAPI_AVEncMPVGOPSize, gop);
                set_codec(&api, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
            }

            let size = ((cap.out_w as u64) << 32) | cap.out_h as u64;
            let rate = ((fps as u64) << 32) | 1;
            let ot = MFCreateMediaType().map_err(e("MFCreateMediaType"))?;
            ot.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("out major"))?;
            ot.SetGUID(&MF_MT_SUBTYPE, &subtype).map_err(e("out subtype"))?;
            ot.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).map_err(e("out bitrate"))?;
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
            Ok(Encoder { mft, api, events, _mgr: mgr })
        }
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
        let fps = o.fps;
        let gop = o.gop;
        let mut cap = Capture::new(pool, &o, log)?;
        log(&format!("gpu: capturing {}x{} -> {}x{} @{}fps (zero-copy)", cap.width, cap.height, cap.out_w, cap.out_h, fps));
        let enc = Encoder::new(&cap, fps, o.bitrate_mbps * 1_000_000, gop, &o.codec, log)?;

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
                if n % gop as u64 == 0 {
                    enc.force_key();
                }
                enc.feed(tex, n as i64 * dur, dur)?;
                n += 1;
            } else if kind == METransformHaveOutput.0 as u32 {
                out.clear();
                enc.drain(&mut out)?;
                if !out.is_empty() && !write_all(pipe, &out) {
                    return Ok(()); // ffmpeg exited
                }
            }
        }
        Ok(())
    })();
    if let Err(err) = res {
        log(&format!("gpu: failed: {err}"));
    }
    unsafe {
        let _ = CloseHandle(pipe);
    }
}
