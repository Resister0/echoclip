//! Chat A: Echo's tray icon + menu (Open Echo / Save clip / Start / stop recording / Quit; left click = open).
//! Windows: tray-icon (the crate Tauri used for Echo's tray; it lives on the UI thread, whose message loop winit
//! runs). Linux: ksni (StatusNotifierItem over D-Bus - KDE, GNOME with the AppIndicator extension).
//! Menu clicks go through rec_core's event queue / worker threads, never straight into the UI.
use crate::rec_core::{self, Ev};
use std::sync::atomic::{AtomicBool, Ordering};

static EXISTS: AtomicBool = AtomicBool::new(false);

pub fn exists() -> bool {
    EXISTS.load(Ordering::Relaxed)
}

fn icon_rgba(size: u32) -> Option<(Vec<u8>, u32)> {
    let png = include_bytes!("../../src-tauri/icons/icon.png");
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let img = img.resize_exact(size, size, image::imageops::FilterType::Triangle).to_rgba8();
    Some((img.into_raw(), size))
}

fn menu_action(id: &str) {
    match id {
        "open" => rec_core::push(Ev::Show),
        "save" => { std::thread::spawn(|| { let _ = rec_core::do_save(None); }); }
        "long" => { std::thread::spawn(|| { let _ = rec_core::do_session(); }); }
        "quit" => rec_core::push(Ev::Quit),
        _ => {}
    }
}

#[cfg(windows)]
thread_local! {
    static TRAY: std::cell::RefCell<Option<tray_icon::TrayIcon>> = const { std::cell::RefCell::new(None) };
}

/// UI thread, once.
pub fn start() {
    if std::env::var("ECHO_NO_TRAY").is_ok() {
        return;
    }
    #[cfg(windows)]
    {
        use tray_icon::menu::{Menu, MenuEvent, MenuItem};
        use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
        let menu = Menu::new();
        let rec_ok = rec_core::can_record();
        let items = [
            MenuItem::with_id("open", "Open Echo", true, None),
            MenuItem::with_id("save", "Save clip", rec_ok, None),
            MenuItem::with_id("long", "Start / stop recording", rec_ok, None),
            MenuItem::with_id("quit", "Quit", true, None),
        ];
        let _ = menu.append_items(&[&items[0], &items[1], &items[2], &items[3]]);
        MenuEvent::set_event_handler(Some(|e: MenuEvent| menu_action(e.id.0.as_str())));
        TrayIconEvent::set_event_handler(Some(|e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                rec_core::push(Ev::Show);
            }
        }));
        let mut b = TrayIconBuilder::new().with_tooltip("Echo").with_menu(Box::new(menu)).with_menu_on_left_click(false);
        if let Some((rgba, s)) = icon_rgba(32) {
            if let Ok(i) = tray_icon::Icon::from_rgba(rgba, s, s) {
                b = b.with_icon(i);
            }
        }
        match b.build() {
            Ok(t) => {
                TRAY.with(|x| *x.borrow_mut() = Some(t));
                EXISTS.store(true, Ordering::Relaxed);
            }
            Err(e) => echo_core::recorder::log_line(&format!("tray: {e}")),
        }
    }
    #[cfg(target_os = "linux")]
    linux::start();
}

pub fn stop() {
    #[cfg(windows)]
    TRAY.with(|x| x.borrow_mut().take());
    #[cfg(target_os = "linux")]
    linux::stop();
    EXISTS.store(false, Ordering::Relaxed);
}

#[cfg(target_os = "linux")]
mod linux {
    use ksni::blocking::TrayMethods;
    use std::sync::Mutex;

    struct EchoTray {
        icon: Vec<ksni::Icon>,
        rec_ok: bool,
    }
    static HANDLE: Mutex<Option<ksni::blocking::Handle<EchoTray>>> = Mutex::new(None);

    impl ksni::Tray for EchoTray {
        fn id(&self) -> String {
            "link.echoclip.Echo".into()
        }
        fn title(&self) -> String {
            "Echo".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            self.icon.clone()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip { title: "Echo".into(), ..Default::default() }
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            super::menu_action("open");
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::StandardItem;
            let item = |label: &str, id: &'static str, enabled: bool| -> ksni::MenuItem<Self> {
                StandardItem { label: label.into(), enabled, activate: Box::new(move |_: &mut Self| super::menu_action(id)), ..Default::default() }.into()
            };
            vec![
                item("Open Echo", "open", true),
                item("Save clip", "save", self.rec_ok),
                item("Start / stop recording", "long", self.rec_ok),
                ksni::MenuItem::Separator,
                item("Quit", "quit", true),
            ]
        }
    }

    pub fn start() {
        let icon = super::icon_rgba(64)
            .map(|(rgba, s)| {
                // RGBA -> ARGB32 (network byte order)
                let mut data = Vec::with_capacity(rgba.len());
                for p in rgba.chunks_exact(4) {
                    data.extend_from_slice(&[p[3], p[0], p[1], p[2]]);
                }
                vec![ksni::Icon { width: s as i32, height: s as i32, data }]
            })
            .unwrap_or_default();
        let t = EchoTray { icon, rec_ok: crate::rec_core::can_record() };
        // Flatpak can't own the name without a permission: the version without a bus name works everywhere
        let r = if echo_core::linuxutil::in_flatpak() { t.disable_dbus_name(true).spawn() } else { t.spawn() };
        match r {
            Ok(h) => {
                *HANDLE.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
                super::EXISTS.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Err(e) => echo_core::recorder::log_line(&format!("tray: {e}")),
        }
    }
    pub fn stop() {
        if let Some(h) = HANDLE.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.shutdown().wait();
        }
    }
}
