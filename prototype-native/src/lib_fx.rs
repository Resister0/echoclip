//! Chat B (night, 9 Nov): Settings > Appearance > Background effect = Echo's bgFx (ui/index.html, chat 30), ported 1:1:
//! Off / Snow / Stars / Fireflies / Embers / Leaves / Rain + "Still" (one frame, never animates). Same particle counts,
//! speeds and colours. ~30 fps; draws NOTHING while Echo isn't in front (focus / minimized / hidden = while you play)
//! and while the player or the editor is open, like Echo. Per PC like Echo's localStorage: kept in
//! %APPDATA%\Clipper\native_ui.json (not settings.json - Echo's recorder rewrites that file).
use crate::AppWindow;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Model, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
struct P {
    x: f32,
    y: f32,
    r: f32,
    vx: f32,
    vy: f32,
    p: f32,
    s: f32,
}

#[derive(Default)]
struct State {
    kind: String,
    still: bool,
    parts: Vec<P>,
    size: (f32, f32),
    /// a still picture was drawn for this size / kind already
    drawn: bool,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
    static MODEL: Rc<VecModel<crate::FxPart>> = Rc::new(VecModel::default());
}

fn file() -> std::path::PathBuf {
    crate::settings::path().with_file_name("native_ui.json")
}

/// any other per-PC UI value in native_ui.json (Echo keeps these in its WebView's localStorage): Home's Get started
/// skips / hide / "copied once"
pub fn ui_get(key: &str) -> serde_json::Value {
    // r2-threads (L29): parsed again only when the file changed
    crate::library::json_cached(&file()).map(|v| v[key].clone()).unwrap_or(serde_json::Value::Null)
}
pub fn ui_set(key: &str, val: serde_json::Value) {
    let p = file();
    let mut v: serde_json::Value = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| serde_json::json!({}));
    if val.is_null() {
        if let Some(m) = v.as_object_mut() {
            m.remove(key);
        }
    } else {
        v[key] = val;
    }
    let _ = std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap_or_default());
}

fn load() -> (String, bool) {
    let v: serde_json::Value = std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    (v["bgfx"].as_str().unwrap_or("").to_string(), v["bgfx_still"].as_bool().unwrap_or(false))
}

fn save(kind: &str, still: bool) {
    let p = file();
    let mut v: serde_json::Value = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| serde_json::json!({}));
    v["bgfx"] = serde_json::json!(kind);
    v["bgfx_still"] = serde_json::json!(still);
    let _ = std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap_or_default());
}

/// for settings.rs: (kind, still)
pub fn state() -> (String, bool) {
    ST.with(|s| {
        let s = s.borrow();
        (s.kind.clone(), s.still)
    })
}

/// S.set hook (main.rs): true = it was one of ours ("bgfx" / "bgfx_still")
pub fn set(key: &str, val: &str) -> bool {
    match key {
        "bgfx" | "bgfx_still" => {
            ST.with(|s| {
                let mut s = s.borrow_mut();
                if key == "bgfx" {
                    s.kind = val.to_string();
                } else {
                    s.still = val == "true";
                }
                s.parts.clear(); // made again for the new kind on the next tick
                s.drawn = false;
                save(&s.kind, s.still);
            });
            true
        }
        _ => false,
    }
}

fn rnd(a: f32, b: f32) -> f32 {
    use std::cell::Cell;
    thread_local!(static SEED: Cell<u64> = Cell::new(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(7, |d| d.as_nanos() as u64) | 1));
    let x = SEED.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        x
    });
    a + (x >> 11) as f32 / (1u64 << 53) as f32 * (b - a)
}

/// Echo's make()
fn make(kind: &str, w: f32, h: f32) -> Vec<P> {
    let n = match kind {
        "stars" => 90,
        "fireflies" => 26,
        "leaves" => 22,
        "rain" => 110,
        _ => 70,
    };
    (0..n)
        .map(|_| P {
            x: rnd(0.0, w),
            y: rnd(0.0, h),
            r: match kind {
                "stars" => rnd(0.5, 1.6),
                "fireflies" => rnd(1.5, 3.0),
                "leaves" => rnd(4.0, 7.0),
                _ => rnd(1.0, 2.6),
            },
            vx: if kind == "rain" { -1.2 } else { rnd(-0.3, 0.3) },
            vy: match kind {
                "embers" => -rnd(0.3, 1.0),
                "stars" => 0.0,
                "fireflies" => rnd(-0.3, 0.3),
                "rain" => rnd(9.0, 14.0),
                _ => rnd(0.4, 1.2),
            },
            p: rnd(0.0, 6.28),
            s: rnd(0.01, 0.04),
        })
        .collect()
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> slint::Color {
    slint::Color::from_argb_u8((a.clamp(0.0, 1.0) * 255.0) as u8, r as u8, g as u8, b as u8)
}

/// Echo's frame(): move (unless Still) and turn every particle into a shape
fn step(kind: &str, still: bool, parts: &mut [P], w: f32, h: f32, accent: slint::Color) -> Vec<crate::FxPart> {
    let mut out = Vec::with_capacity(parts.len());
    for q in parts.iter_mut() {
        if !still {
            q.x += q.vx;
            q.y += q.vy;
            q.p += q.s;
            if kind == "fireflies" {
                q.vx += rnd(-0.03, 0.03);
                q.vy += rnd(-0.03, 0.03);
                q.vx *= 0.98;
                q.vy *= 0.98;
            }
        }
        if q.y > h + 10.0 {
            q.y = -10.0;
        }
        if q.y < -10.0 {
            q.y = h + 10.0;
        }
        if q.x > w + 10.0 {
            q.x = -10.0;
        }
        if q.x < -10.0 {
            q.x = w + 10.0;
        }
        let dot = |cx: f32, cy: f32, r: f32, col: slint::Color, glow: bool| crate::FxPart {
            x: cx - r,
            y: cy - r,
            w: r * 2.0,
            h: r * 2.0,
            rot: 0.0,
            col,
            glow,
        };
        out.push(match kind {
            "snow" => dot(q.x + q.p.sin() * 6.0, q.y, q.r, rgba(255.0, 255.0, 255.0, 0.8), false),
            "stars" => dot(q.x, q.y, q.r, rgba(255.0, 255.0, 255.0, 0.35 + 0.45 * q.p.sin().abs()), false),
            "fireflies" => dot(q.x, q.y, q.r, rgba(255.0, 228.0, 138.0, 0.4 + 0.5 * q.p.sin().abs()), true),
            "embers" => dot(q.x + q.p.sin() * 4.0, q.y, q.r, rgba(255.0, 120.0 + (q.r * 30.0).floor(), 60.0, 0.5 + 0.4 * q.p.sin()), false),
            // a 1 px streak from (x, y) back along the fall direction (Echo: lineTo(x + vx*1.6, y - vy*1.6))
            "rain" => {
                let (dx, dy) = (q.vx * 1.6, -q.vy * 1.6);
                let len = (dx * dx + dy * dy).sqrt();
                crate::FxPart {
                    x: q.x + dx / 2.0 - 0.5,
                    y: q.y + dy / 2.0 - len / 2.0,
                    w: 1.0,
                    h: len,
                    rot: -dx.atan2(-dy).to_degrees(),
                    col: rgba(180.0, 200.0, 255.0, 0.35),
                    glow: false,
                }
            }
            // an ellipse r x r/2.2 in the accent colour, swaying and turning (Echo: alpha .55)
            _ => {
                let (rw, rh) = (q.r * 2.0, q.r / 2.2 * 2.0);
                crate::FxPart {
                    x: q.x + q.p.sin() * 14.0 - rw / 2.0,
                    y: q.y - rh / 2.0,
                    w: rw,
                    h: rh,
                    rot: q.p.to_degrees(),
                    col: accent.with_alpha(0.55),
                    glow: false,
                }
            }
        });
    }
    out
}

pub fn wire(app: &AppWindow) {
    let (kind, still) = load();
    ST.with(|s| {
        let mut s = s.borrow_mut();
        s.kind = kind;
        s.still = still;
    });
    MODEL.with(|m| app.global::<crate::Fx>().set_parts(m.clone().into()));
    let w = app.as_weak();
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(33), move || {
        let Some(a) = w.upgrade() else { return };
        tick(&a);
    });
    crate::idle::keep(t); // pauses while Echo is hidden in the tray
}

fn tick(a: &AppWindow) {
    let fx = a.global::<crate::Fx>();
    let (kind, still) = state();
    let win = a.window();
    // Echo's busy(): not in front (= while you play), or the player / editor is open
    let focused = win.with_winit_window(|w| w.has_focus()).unwrap_or(true);
    let busy = kind.is_empty() || !focused || win.is_minimized() || !win.is_visible() || a.get_player_open() || a.global::<crate::Ed>().get_open();
    if busy {
        if fx.get_on() {
            fx.set_on(false);
        }
        return;
    }
    let sz = win.size().to_logical(win.scale_factor());
    let (w, h) = (sz.width, sz.height);
    let bg = a.global::<crate::T>().get_bg();
    fx.set_light((bg.red() as u32 + bg.green() as u32 + bg.blue() as u32) > 3 * 150);
    let accent = a.global::<crate::T>().get_accent2();
    let shapes = ST.with(|s| {
        let mut s = s.borrow_mut();
        if s.parts.is_empty() || (s.size.0 - w).abs() > 1.0 || (s.size.1 - h).abs() > 1.0 {
            // Echo: made again on resize
            s.parts = make(&kind, w, h);
            s.size = (w, h);
            s.drawn = false;
        }
        if still && s.drawn {
            return None;
        }
        s.drawn = true;
        Some(step(&kind, still, &mut s.parts, w, h, accent))
    });
    if let Some(shapes) = shapes {
        MODEL.with(|m| {
            if m.row_count() == shapes.len() {
                for (i, p) in shapes.into_iter().enumerate() {
                    m.set_row_data(i, p);
                }
            } else {
                m.set_vec(shapes);
            }
        });
    }
    if !fx.get_on() {
        fx.set_on(true);
    }
}
