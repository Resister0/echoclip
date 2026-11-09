//! Chat B: colour picker for custom themes (Settings > Appearance > Your own colours) + RGB mode.
//! Picker = Echo's colour input (square for saturation / brightness, hue bar, R G B). Double-click the round
//! preview = that colour cycles through the rainbow (William: "like Vape v4"); settings.json "theme_rgb" = the
//! keys that cycle (theme_accent / theme_bg / theme_hi). The stored colour keeps its saturation + brightness, only
//! the hue turns (one turn every 6 s). Only drawn while Echo shows a custom theme; the file is never written per frame.
use crate::settings::Store;
use crate::{AppWindow, Cp, T};
use slint::{Color, ComponentHandle};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn parse(h: &str) -> (f32, f32, f32) {
    let h = h.trim().trim_start_matches('#');
    let n = u32::from_str_radix(h, 16).unwrap_or(0x7c5cff);
    (((n >> 16) & 255) as f32 / 255.0, ((n >> 8) & 255) as f32 / 255.0, (n & 255) as f32 / 255.0)
}
fn to_hex(r: f32, g: f32, b: f32) -> String {
    format!("#{:02x}{:02x}{:02x}", (r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8)
}
pub fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    let h = if d == 0.0 {
        0.0
    } else if mx == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if mx == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if mx == 0.0 { 0.0 } else { d / mx }, mx)
}
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}
fn col(r: f32, g: f32, b: f32) -> Color {
    Color::from_rgb_u8((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

fn rgb_keys(st: &Store) -> Vec<String> {
    st.v.get("theme_rgb").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// the theme with these colours (hex) put into T right now - like main.rs apply_theme, without saving anything
fn apply_live(a: &AppWindow, st: &Store, over: &[(String, String)]) {
    let get = |k: &str| over.iter().find(|x| x.0 == k).map(|x| x.1.clone()).unwrap_or_else(|| st.s(k));
    let p = crate::settings::theme_vars(&st.s("theme"), &get("theme_accent"), &get("theme_bg"), &get("theme_hi"));
    let t = a.global::<T>();
    t.set_bg(p.bg);
    t.set_rail(p.rail);
    t.set_panel(p.panel);
    t.set_panel2(p.panel2);
    t.set_line(p.line);
    t.set_hover(p.hover);
    t.set_text(p.text);
    t.set_muted(p.muted);
    t.set_dim(p.dim);
    t.set_accent(p.accent);
    t.set_accent2(p.accent2);
    t.set_hi(p.hi);
    t.set_g0(p.grad[0]);
    t.set_g1(p.grad[1]);
    t.set_g2(p.grad[2]);
}

thread_local! {
}

fn show(a: &AppWindow, st: &Store, key: &str, h: f32, s: f32, v: f32) {
    let cp = a.global::<Cp>();
    let (r, g, b) = hsv_to_rgb(h, s, v);
    let (hr, hg, hbb) = hsv_to_rgb(h, 1.0, 1.0);
    cp.set_h(h);
    cp.set_s(s);
    cp.set_v(v);
    cp.set_cur(col(r, g, b));
    cp.set_hue_color(col(hr, hg, hbb));
    cp.set_r((r * 255.0).round() as i32);
    cp.set_g((g * 255.0).round() as i32);
    cp.set_b((b * 255.0).round() as i32);
    cp.set_rgb(rgb_keys(st).iter().any(|k| k == key));
}

pub fn wire(app: &AppWindow, store: Rc<RefCell<Store>>, r: Rc<dyn Fn()>) {
    let cp = app.global::<Cp>();
    {
        let (w, store) = (app.as_weak(), store.clone());
        cp.on_open_at(move |key, x, y| {
            let Some(a) = w.upgrade() else { return };
            let st = store.borrow();
            let (r, g, b) = parse(&st.s(&key));
            let (h, s, v) = rgb_to_hsv(r, g, b);
            let cp = a.global::<Cp>();
            cp.set_key(key.clone());
            cp.set_px(x);
            cp.set_py(y);
            show(&a, &st, &key, h, s, v);
            cp.set_open(true);
        });
    }
    {
        let (w, store, r) = (app.as_weak(), store.clone(), r.clone());
        cp.on_changed(move |h, s, v, fin| {
            let Some(a) = w.upgrade() else { return };
            let key = a.global::<Cp>().get_key().to_string();
            let (cr, cg, cb) = hsv_to_rgb(h, s, v);
            let hex = to_hex(cr, cg, cb);
            show(&a, &store.borrow(), &key, h, s, v);
            if fin {
                let _ = store.borrow_mut().set(&key, &hex);
                r();
            } else {
                apply_live(&a, &store.borrow(), &[(key, hex)]);
            }
        });
    }
    {
        let (w, store, r) = (app.as_weak(), store.clone(), r.clone());
        cp.on_set_rgb(move |cr, cg, cb| {
            let Some(a) = w.upgrade() else { return };
            let key = a.global::<Cp>().get_key().to_string();
            let c = |x: i32| x.clamp(0, 255) as f32 / 255.0;
            let hex = to_hex(c(cr), c(cg), c(cb));
            let _ = store.borrow_mut().set(&key, &hex);
            r();
            let (h, s, v) = rgb_to_hsv(c(cr), c(cg), c(cb));
            show(&a, &store.borrow(), &key, h, s, v);
        });
    }
    {
        let (w, store, r) = (app.as_weak(), store.clone(), r.clone());
        cp.on_toggle_rgb(move || {
            let Some(a) = w.upgrade() else { return };
            let key = a.global::<Cp>().get_key().to_string();
            let mut keys = rgb_keys(&store.borrow());
            let on = if let Some(p) = keys.iter().position(|k| *k == key) {
                keys.remove(p);
                false
            } else {
                keys.push(key.clone());
                true
            };
            let _ = store.borrow_mut().write(&[("theme_rgb", serde_json::json!(keys))]);
            a.global::<Cp>().set_rgb(on);
            r();
            crate::toast(&a, if on { "RGB on - this colour cycles through the rainbow" } else { "RGB off" });
        });
    }
    // RGB mode: ~30 fps hue turn while the custom theme is shown and the window is visible
    let t0 = Instant::now();
    let t = slint::Timer::default();
    let w = app.as_weak();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(33), move || {
        let Some(a) = w.upgrade() else { return };
        let st = store.borrow();
        if st.s("theme") != "custom" || !a.window().is_visible() || a.window().is_minimized() {
            return;
        }
        let keys = rgb_keys(&st);
        if keys.is_empty() {
            return;
        }
        let turn = (t0.elapsed().as_secs_f32() * 60.0) % 360.0; // one turn every 6 s
        let over: Vec<(String, String)> = keys
            .iter()
            .map(|k| {
                let (r, g, b) = parse(&st.s(k));
                let (h, s, v) = rgb_to_hsv(r, g, b);
                let s = if s < 0.25 { 0.85 } else { s };
                let (r, g, b) = hsv_to_rgb((h + turn) % 360.0, s, v.max(0.35));
                (k.clone(), to_hex(r, g, b))
            })
            .collect();
        apply_live(&a, &st, &over);
        // the open picker follows its colour
        let cp = a.global::<Cp>();
        if cp.get_open() {
            if let Some((_, hx)) = over.iter().find(|x| x.0 == cp.get_key().as_str()) {
                let (r, g, b) = parse(hx);
                cp.set_cur(col(r, g, b));
            }
        }
    });
    crate::idle::keep(t); // pauses while Echo is hidden in the tray
}
