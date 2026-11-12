//! r2-layout (round 2, 10 Nov 2026): menus that go over the video, and clicks that close a menu and still go through.
//! Slint side: ui/over.slint.
//!
//! L1 (Windows, video in its own D3D11 child window): Slint can't draw over that window, so a menu that reaches into
//! the video (player Copy ▾ / Share / Cloud / MP3, the header's clip-length list, editor Sticker / Export / timeline
//! menus) was hidden behind it. Window regions can't help with the flip model (mpv's default, it ignores regions).
//! So while such a menu is open the video window is parked (like for a dialog) and Slint shows a still of the
//! current picture in its place - the menu draws over that like over any Slint item. Sound keeps playing.
//! Tooltips don't park the video: TipLayer puts them on the side away from the video (ui/theme.slint).
//!
//! L39: Echo closes its editor menus on pointerdown and the click still reaches what's under it. Slint's full-window
//! catcher swallows that press, so the catcher closes the menu and calls Over.pass; the same press is sent again
//! here, one event-loop turn later (the catcher is gone by then, so the press lands on what's under it).
use crate::{AppWindow, Over};
use slint::ComponentHandle;
#[cfg(windows)]
use std::cell::Cell;
use std::time::Duration;

pub fn init(app: &AppWindow) {
    // Over.win-w / win-h: the window's logical size (L10, L34) - polled, like titlebar.rs does for win-hh
    fn win_size(a: &AppWindow) {
        let s = a.window().size().to_logical(a.window().scale_factor());
        let o = a.global::<Over>();
        if s.width > 0.0 && (o.get_win_w() - s.width).abs() > 0.5 {
            o.set_win_w(s.width);
        }
        if s.height > 0.0 && (o.get_win_h() - s.height).abs() > 0.5 {
            o.set_win_h(s.height);
        }
    }
    win_size(app);
    let t = Box::leak(Box::new(slint::Timer::default()));
    let w = app.as_weak();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
        if let Some(a) = w.upgrade() {
            win_size(&a);
        }
    });
    let w = app.as_weak();
    app.global::<Over>().on_pass(move |x, y, b| {
        let w = w.clone();
        slint::Timer::single_shot(Duration::ZERO, move || {
            let Some(a) = w.upgrade() else { return };
            let button = match b {
                1 => slint::platform::PointerEventButton::Right,
                2 => slint::platform::PointerEventButton::Middle,
                _ => slint::platform::PointerEventButton::Left,
            };
            a.window().dispatch_event(slint::platform::WindowEvent::PointerPressed { position: slint::LogicalPosition::new(x, y), button });
        });
    });
}

/// a Slint menu / popup is open that may reach over the video: any PopupWindow (except Slint's own tooltips), the
/// cloud menu layer, the editor's own menus
#[cfg(windows)]
pub fn menu_open(a: &AppWindow) -> bool {
    use slint::private_unstable_api::re_exports::{WindowInner, WindowKind};
    // 11 Nov 2026: real popups (PopupWindow) are shown over the LIVE video by menuwin.rs - no parking, no still -
    // unless that doesn't work on this PC (menuwin::broken). The cloud menu layer and the editor's own menus are
    // plain rectangles: those still park the video.
    let popups = crate::menuwin::broken() && WindowInner::from_pub(a.window()).active_popups().iter().any(|p| !matches!(p.window_kind, WindowKind::ToolTip));
    let ed = a.global::<crate::Ed>();
    popups || a.global::<crate::Cm>().get_open() || (ed.get_open() && (ed.get_sticker_menu() || ed.get_export_menu() || ed.get_menu_open()))
}

#[cfg(windows)]
thread_local! {
    /// a menu was open over the video at the last frame
    static PARKED: Cell<bool> = const { Cell::new(false) };
}

/// Called every frame in Windows window mode. `shown` = the video window is on screen right now. Returns true while a
/// menu is open: the caller parks the video window then. On the first frame of a menu, the picture on screen is
/// grabbed for the still (before the window goes away); when the menu closes the still goes.
#[cfg(windows)]
pub fn park_for_menu(a: &AppWindow, m: &crate::mpv::Mpv, shown: bool) -> bool {
    let menu = menu_open(a);
    let o = a.global::<Over>();
    if menu && !PARKED.with(|p| p.replace(true)) {
        if shown {
            // "window" = exactly what is on screen, incl. the editor's texts / boxes (an mpv overlay); "video" = the bare frame
            let shot = m.screenshot_raw("window").or_else(|| m.screenshot_raw("video"));
            if let Some((w, h, rgb)) = shot {
                let buf = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(&rgb, w, h);
                o.set_still(slint::Image::from_rgb8(buf));
                o.set_still_on(true);
            }
        }
    } else if !menu && PARKED.with(|p| p.replace(false)) {
        o.set_still_on(false);
        o.set_still(slint::Image::default());
    }
    menu
}

/// 12 Nov 2026: the player closed - a still shown for a menu goes too (park_for_menu only runs while the player is
/// open, so a menu closing together with the player left it on, over the next clip's video)
pub fn player_closed(a: &AppWindow) {
    #[cfg(windows)]
    PARKED.with(|p| p.set(false));
    let o = a.global::<Over>();
    o.set_still_on(false);
    o.set_still(slint::Image::default());
}

/// tooltips keep off the video window (TipLayer reads Over.video-win)
#[cfg(windows)]
pub fn set_video_win(a: &AppWindow, on: bool) {
    let o = a.global::<Over>();
    if o.get_video_win() != on {
        o.set_video_win(on);
    }
}
