//! Chat B (night, 9 Nov): Settings > Echo Profile editing = Echo's renderProfile (ui/index.html, chat 30), all through
//! Echo's own Recorder::ec_profile / profile_avatar (echo-core): GET /profile/me, POST /profile (make / save),
//! /profile/avatar (picture, remove), /profile/pin, /follow, /profile/delete. Pictures (avatar, share thumbnails,
//! friends) are fetched in the background and filled in when they arrive. UI: ui/lib_profile.slint (global Pf).
use crate::rec_core::rec;
use crate::{AppWindow, PfClip, PfFriend, PfSwatch};
use serde_json::{json, Value};
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::rc::Rc;

const PF_COLORS: [&str; 8] = ["#7c5cff", "#2fa8e0", "#2fbf86", "#f07a1a", "#e0609e", "#d0505a", "#ffd23f", "#8a96a8"];
const PAGE: &str = "https://echoclip.link/u/";

fn hex(s: &str) -> slint::Color {
    let h = s.trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(0x7c5cff);
    slint::Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn signed_in() -> bool {
    crate::library::echo_settings()
        .and_then(|v| v.get("cloud_accounts").cloned())
        .and_then(|a| a.as_array().cloned())
        .is_some_and(|a| a.iter().any(|x| x["provider"] == "echocloud"))
}

/// a picture from the internet (decoded on the worker thread), then handed to the UI as RGBA
fn fetch(url: String, done: impl FnOnce(Image) + 'static) {
    if url.is_empty() {
        return;
    }
    crate::lib_net::bg(
        move || {
            let b = crate::lib_net::get_bytes(&url).ok()?;
            let i = image::load_from_memory(&b).ok()?.thumbnail(480, 480).to_rgba8();
            Some((i.width(), i.height(), i.into_raw()))
        },
        move |r| {
            if let Some((w, h, px)) = r {
                done(Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&px, w, h)));
            }
        },
    );
}

/// the card picture of a clip on this PC: by the share's cloud key ("Echo/FiveM/Evidence/Evidence.mp4"), else by its
/// title when that is a clip's file name (the server's default title)
fn local_thumb(key: &str, title: &str) -> Option<Image> {
    let lib = crate::LIB.with(|l| l.borrow().clone())?;
    let by_key = key
        .strip_prefix("Echo/")
        .filter(|r| !r.is_empty())
        .and_then(|r| lib.index_of_path(&lib.root().join(r.replace('\\', "/"))));
    let idx = by_key.or_else(|| {
        if title.is_empty() {
            return None;
        }
        let hits: Vec<_> = lib.all().iter().filter(|c| c.path.file_stem().is_some_and(|s| s.to_string_lossy() == title)).map(|c| c.path.clone()).collect();
        // only when the name is unique - a wrong picture is worse than none
        if hits.len() == 1 {
            lib.index_of_path(&hits[0])
        } else {
            None
        }
    })?;
    lib.thumb_img(idx)
}

pub fn wire(app: &AppWindow) {
    let p = app.global::<crate::Pf>();
    p.set_swatches(ModelRc::new(VecModel::from(PF_COLORS.iter().map(|c| PfSwatch { hex: (*c).into(), c: hex(c) }).collect::<Vec<_>>())));
    // Echo: @name = lower case, a-z 0-9 _ only, max 20
    p.on_clean(|t| t.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(20).collect::<String>().into());
    let w = app.as_weak();
    p.on_act(move |what, arg| {
        if let Some(a) = w.upgrade() {
            act(&a, what.as_str(), arg.to_string());
        }
    });
}

/// Settings > Echo Profile opened (main.rs S.pick hook)
pub fn open(a: &AppWindow) {
    load(a);
}

fn load(a: &AppWindow) {
    let p = a.global::<crate::Pf>();
    let Some(rc) = rec() else { return };
    if !signed_in() {
        p.set_state("out".into());
        return;
    }
    if p.get_state() != "ready" {
        p.set_state("loading".into());
    }
    let w = a.as_weak();
    crate::lib_net::bg(move || rc.ec_profile("GET", "/profile/me", None), move |res| {
        let Some(a) = w.upgrade() else { return };
        let p = a.global::<crate::Pf>();
        let v = match res {
            Ok(v) => v,
            Err(e) => {
                p.set_err(e.into());
                p.set_state("error".into());
                return;
            }
        };
        let pr = &v["profile"];
        if pr.is_null() {
            p.set_state("new".into());
            return;
        }
        let s = |k: &str| pr[k].as_str().unwrap_or("").to_string();
        let (name, handle, color) = (s("name"), s("handle"), if s("color").is_empty() { "#7c5cff".into() } else { s("color") });
        p.set_saved_name(name.clone().into());
        p.set_saved_handle(handle.clone().into());
        p.set_name(name.clone().into());
        p.set_handle(handle.clone().into());
        p.set_bio(s("bio").into());
        p.set_color(color.clone().into());
        p.set_col(hex(&color));
        p.set_letter(name.chars().next().or_else(|| handle.chars().next()).unwrap_or('?').to_uppercase().to_string().into());
        let n = v["followers"].as_u64().unwrap_or(0);
        p.set_followers(format!("{n} follower{}", if n == 1 { "" } else { "s" }).into());
        p.set_pin_max(v["pin_max"].as_i64().unwrap_or(6) as i32);
        // picture
        let av = s("avatar");
        p.set_has_pic(false);
        {
            let w = a.as_weak();
            fetch(av, move |img| {
                if let Some(a) = w.upgrade() {
                    let p = a.global::<crate::Pf>();
                    p.set_pic(img);
                    p.set_has_pic(true);
                }
            });
        }
        // share links: pinned = "On your page", the rest = "Your share links"; thumbnails arrive later
        let shares = v["shares"].as_array().cloned().unwrap_or_default();
        let clip = |x: &Value| PfClip { id: x["id"].as_str().unwrap_or("").into(), title: x["title"].as_str().unwrap_or("").into(), thumb: Image::default(), has_thumb: false };
        let on: Vec<&Value> = shares.iter().filter(|x| x["pinned"].as_bool().unwrap_or(false)).collect();
        let off: Vec<&Value> = shares.iter().filter(|x| !x["pinned"].as_bool().unwrap_or(false)).collect();
        let on_m = Rc::new(VecModel::from(on.iter().map(|x| clip(x)).collect::<Vec<_>>()));
        let off_m = Rc::new(VecModel::from(off.iter().map(|x| clip(x)).collect::<Vec<_>>()));
        p.set_on_page(on_m.clone().into());
        p.set_links(off_m.clone().into());
        for (list, model) in [(on, on_m), (off, off_m)] {
            for (i, x) in list.iter().enumerate() {
                let model = model.clone();
                // 10 Nov: a share link without a picture on the server (made on another PC / an older Echo)
                // shows the clip's own picture from this PC - found by its cloud key ("Echo/<path>") or its name
                if x["thumb"].as_str().unwrap_or("").is_empty() {
                    if let Some(img) = local_thumb(x["key"].as_str().unwrap_or(""), x["title"].as_str().unwrap_or("")) {
                        if let Some(mut c) = model.row_data(i) {
                            c.thumb = img;
                            c.has_thumb = true;
                            model.set_row_data(i, c);
                        }
                    }
                    continue;
                }
                fetch(x["thumb"].as_str().unwrap_or("").to_string(), move |img| {
                    if let Some(mut c) = model.row_data(i) {
                        c.thumb = img;
                        c.has_thumb = true;
                        model.set_row_data(i, c);
                    }
                });
            }
        }
        // following
        let fl = v["following"].as_array().cloned().unwrap_or_default();
        let fm = Rc::new(VecModel::from(
            fl.iter()
                .map(|f| {
                    let (fname, fh) = (f["name"].as_str().unwrap_or("").to_string(), f["handle"].as_str().unwrap_or("").to_string());
                    PfFriend {
                        handle: fh.clone().into(),
                        name: fname.clone().into(),
                        initial: fname.chars().next().or_else(|| fh.chars().next()).unwrap_or('?').to_uppercase().to_string().into(),
                        col: hex(f["color"].as_str().unwrap_or("#7c5cff")),
                        pic: Image::default(),
                        has_pic: false,
                    }
                })
                .collect::<Vec<_>>(),
        ));
        p.set_following(fm.clone().into());
        for (i, f) in fl.iter().enumerate() {
            let fm = fm.clone();
            fetch(f["avatar"].as_str().unwrap_or("").to_string(), move |img| {
                if let Some(mut x) = fm.row_data(i) {
                    x.pic = img;
                    x.has_pic = true;
                    fm.set_row_data(i, x);
                }
            });
        }
        p.set_state("ready".into());
    });
}

/// one profile call on a worker thread; ok = toast (if any) + reload
fn call(a: &AppWindow, method: &'static str, path: &'static str, body: Value, ok: impl FnOnce(&AppWindow, &Value) + 'static) {
    let Some(rc) = rec() else { return };
    let w = a.as_weak();
    crate::lib_net::bg(move || rc.ec_profile(method, path, Some(body)), move |res| {
        let Some(a) = w.upgrade() else { return };
        match res {
            Ok(v) => {
                ok(&a, &v);
                load(&a);
            }
            Err(e) => crate::toast(&a, e),
        }
    });
}

fn act(a: &AppWindow, what: &str, arg: String) {
    let p = a.global::<crate::Pf>();
    match what {
        "retry" => load(a),
        "make" => {
            let color = crate::library::echo_settings().and_then(|v| v["theme_accent"].as_str().map(String::from)).filter(|c| c.starts_with('#')).unwrap_or_else(|| "#7c5cff".into());
            call(a, "POST", "/profile", json!({ "handle": p.get_handle().as_str(), "name": p.get_name().as_str(), "bio": "", "color": color }), |a, _| {
                crate::toast(a, "Profile made - add a picture and pin some clips")
            });
        }
        "save" => call(a, "POST", "/profile", json!({ "handle": p.get_handle().as_str(), "name": p.get_name().as_str(), "bio": p.get_bio().as_str(), "color": p.get_color().as_str() }), |a, _| {
            crate::toast(a, "Profile saved")
        }),
        "pic" => {
            let w = a.as_weak();
            crate::lib_pick::pick_file_ui("Pick a picture", "Pictures", &["png", "jpg", "jpeg", "gif", "webp", "bmp"], move |path| {
                let (Some(a), Some(rc)) = (w.upgrade(), rec()) else { return };
                crate::toast(&a, "Uploading your picture…");
                let w = a.as_weak();
                crate::lib_net::bg(move || rc.profile_avatar(&path.to_string_lossy()), move |res| {
                    let Some(a) = w.upgrade() else { return };
                    match res {
                        Ok(_) => {
                            crate::toast(&a, "Picture updated");
                            load(&a);
                        }
                        Err(e) => crate::toast(&a, e),
                    }
                });
            });
        }
        "pic-x" => call(a, "POST", "/profile/avatar", json!({ "remove": true }), |_, _| {}),
        "view" => crate::open_url(&format!("{PAGE}{}", p.get_saved_handle())),
        "open" => crate::open_url(&format!("{PAGE}{arg}")),
        "pin" | "unpin" => call(a, "POST", "/profile/pin", json!({ "id": arg, "on": what == "pin" }), |_, _| {}),
        "follow" => {
            let h = p.get_follow_text().trim().to_string();
            if h.is_empty() {
                return;
            }
            call(a, "POST", "/follow", json!({ "handle": h, "on": true }), |a, v| {
                a.global::<crate::Pf>().set_follow_text("".into());
                crate::toast(a, format!("Following {}", v["profile"]["name"].as_str().unwrap_or("them")));
            });
        }
        "unfollow" => call(a, "POST", "/follow", json!({ "handle": arg, "on": false }), |_, _| {}),
        "delete" => crate::confirm_box(a, "Delete your profile?", "Your page, picture, pins and follows are removed. Clips and share links stay.", "Delete profile", |a| {
            call(a, "POST", "/profile/delete", json!({}), |a, _| crate::toast(a, "Profile deleted"));
        }),
        _ => {}
    }
}
