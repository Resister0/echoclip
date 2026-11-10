//! wayland-dnd (10 Nov 2026): drag and drop from Dolphin (or any Wayland app) onto Echo's window on Linux Wayland.
//! winit 0.30 has no Wayland drag and drop (only X11 / Windows send HoveredFile / DroppedFile), and Slint 1.18 is
//! still on winit 0.30. So Echo listens itself: a wl_data_device on the SAME Wayland connection as the window
//! (raw-window-handle gives the wl_display), on its own event queue + thread. Nothing else in Echo has a
//! wl_data_device there (winit: none; Slint's clipboard = arboard over wlr-data-control on its own connection).
//! - only `text/uri-list` drags are accepted, and only as COPY (Dolphin offers move too - a move would delete the
//!   file from where it was)
//! - in the Flatpak, Dolphin's / GTK's `application/vnd.portal.filetransfer` is used first: the document portal
//!   hands over paths the sandbox can read (it only sees ~/Videos itself); the plain uri-list is the fallback
//! - the result goes into drop.rs's own steps (overlay, sidebar folder under the mouse, import / editor)
use crate::AppWindow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};

const URIS: &str = "text/uri-list";
const PORTAL: &str = "application/vnd.portal.filetransfer";

/// the mime types one offer has
#[derive(Default)]
struct Mimes(Mutex<Vec<String>>);

impl Mimes {
    fn has(&self, m: &str) -> bool {
        self.0.lock().map(|v| v.iter().any(|x| x == m)).unwrap_or(false)
    }
}

struct St {
    ui: slint::Weak<AppWindow>,
    conn: Connection,
    /// wl_data_device_manager version (3+ = set_actions / finish)
    ver: u32,
    /// the drag over the window now, if it has files
    cur: Option<WlDataOffer>,
    /// last drag position (surface = logical px, like Slint's)
    pos: (f32, f32),
    /// the files were dropped: the Leave that follows must not close the overlay (it would clear the sidebar hit)
    dropped: bool,
    /// a motion is already on its way to the UI thread (motions come at mouse rate: one at a time)
    moving: Arc<Mutex<Option<(f32, f32)>>>,
}

thread_local! {
    static STARTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Called from the winit event hook (main.rs): the first time, on Wayland, starts the listener thread.
pub fn start(wk: &slint::Weak<AppWindow>, win: &slint::Window) {
    if STARTED.with(|s| s.replace(true)) {
        return;
    }
    let wh = win.window_handle();
    let Ok(dh) = raw_window_handle::HasDisplayHandle::display_handle(&wh) else { return };
    let raw_window_handle::RawDisplayHandle::Wayland(h) = dh.as_raw() else { return }; // X11: winit's own DnD
    // SAFETY: the wl_display is winit's, alive for the whole event loop (= Echo's life). "Guest" backend: it
    // never closes the display.
    let backend = unsafe { wayland_backend::client::Backend::from_foreign_display(h.display.as_ptr() as *mut _) };
    let conn = Connection::from_backend(backend);
    let ui = wk.clone();
    let r = std::thread::Builder::new().name("wl-dnd".into()).spawn(move || {
        if let Err(e) = run(conn, ui) {
            eprintln!("[wl-dnd] drag and drop off: {e}");
        }
    });
    if let Err(e) = r {
        eprintln!("[wl-dnd] no thread: {e}");
    }
}

fn run(conn: Connection, ui: slint::Weak<AppWindow>) -> Result<(), String> {
    let (globals, mut q) = registry_queue_init::<St>(&conn).map_err(|e| e.to_string())?;
    let qh = q.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=1, ()).map_err(|e| format!("no seat: {e}"))?;
    let mgr: WlDataDeviceManager = globals.bind(&qh, 1..=3, ()).map_err(|e| format!("no data device manager: {e}"))?;
    let ver = mgr.version();
    let _dev = mgr.get_data_device(&seat, &qh, ());
    let mut st = St { ui, conn, ver, cur: None, pos: (0.0, 0.0), dropped: false, moving: Arc::default() };
    loop {
        q.blocking_dispatch(&mut st).map_err(|e| e.to_string())?;
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for St {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlSeat, ()> for St {
    fn event(_: &mut Self, _: &WlSeat, _: <WlSeat as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataDeviceManager, ()> for St {
    fn event(_: &mut Self, _: &WlDataDeviceManager, _: <WlDataDeviceManager as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataOffer, Mimes> for St {
    fn event(_: &mut Self, _: &WlDataOffer, ev: wl_data_offer::Event, m: &Mimes, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_data_offer::Event::Offer { mime_type } = ev {
            if let Ok(mut v) = m.0.lock() {
                v.push(mime_type);
            }
        }
    }
}

impl Dispatch<WlDataDevice, ()> for St {
    fn event(st: &mut Self, _: &WlDataDevice, ev: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use wl_data_device::Event as E;
        match ev {
            E::Enter { serial, x, y, id, .. } => {
                if let Some(old) = st.cur.take() {
                    old.destroy();
                }
                st.dropped = false;
                let Some(o) = id else { return };
                let files = o.data::<Mimes>().is_some_and(|m| m.has(URIS));
                if !files {
                    o.accept(serial, None); // not files (text, a picture from a browser): no drop here
                    o.destroy();
                    return;
                }
                o.accept(serial, Some(URIS.into()));
                if st.ver >= 3 {
                    o.set_actions(DndAction::Copy, DndAction::Copy);
                }
                st.cur = Some(o);
                st.pos = (x as f32, y as f32);
                let p = st.pos;
                let _ = st.ui.upgrade_in_event_loop(move |a| crate::drop::wl_hover(&a, Some(p)));
            }
            E::Motion { x, y, .. } => {
                if st.cur.is_none() {
                    return;
                }
                st.pos = (x as f32, y as f32);
                let first = st.moving.lock().map(|mut g| g.replace(st.pos).is_none()).unwrap_or(false);
                if first {
                    let mv = st.moving.clone();
                    let _ = st.ui.upgrade_in_event_loop(move |a| {
                        if let Some(p) = mv.lock().ok().and_then(|mut g| g.take()) {
                            crate::drop::wl_hover(&a, Some(p));
                        }
                    });
                }
            }
            E::Leave => {
                if let Some(o) = st.cur.take() {
                    o.destroy();
                }
                if !std::mem::take(&mut st.dropped) {
                    let _ = st.ui.upgrade_in_event_loop(|a| crate::drop::wl_hover(&a, None));
                }
            }
            E::Drop => {
                let Some(o) = st.cur.take() else { return };
                st.dropped = true;
                let (conn, ui, ver, p) = (st.conn.clone(), st.ui.clone(), st.ver, st.pos);
                // read the data off this thread: the dragging app writes it while this queue keeps running
                std::thread::spawn(move || {
                    let paths = files(&o, &conn);
                    if ver >= 3 {
                        o.finish();
                    }
                    o.destroy();
                    let _ = conn.flush();
                    let _ = ui.upgrade_in_event_loop(move |a| crate::drop::wl_drop(&a, paths, p));
                });
            }
            E::Selection { id: Some(o) } => o.destroy(), // the clipboard (Echo reads it through Slint, not here)
            _ => {}
        }
    }

    event_created_child!(St, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, Mimes::default()),
    ]);
}

/// one mime type of the dropped data, read to the end
fn read(o: &WlDataOffer, conn: &Connection, mime: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let (mut r, w) = std::io::pipe().ok()?;
    o.receive(mime.into(), std::os::fd::AsFd::as_fd(&w));
    conn.flush().ok()?;
    drop(w); // only the dragging app holds the write end now: EOF when it's done
    let mut buf = vec![];
    r.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// the dropped files: the portal's (Flatpak) or the uri-list's
fn files(o: &WlDataOffer, conn: &Connection) -> Vec<PathBuf> {
    let has = |m: &str| o.data::<Mimes>().is_some_and(|x| x.has(m));
    if echo_core::linuxutil::in_flatpak() && has(PORTAL) {
        if let Some(key) = read(o, conn, PORTAL) {
            let key = String::from_utf8_lossy(&key).trim_matches(char::from(0)).trim().to_string();
            match portal_files(&key) {
                Ok(v) if !v.is_empty() => return v,
                Ok(_) => {}
                Err(e) => eprintln!("[wl-dnd] document portal: {e} - using the plain file list"),
            }
        }
    }
    read(o, conn, URIS).map(|b| parse_uris(&b)).unwrap_or_default()
}

/// org.freedesktop.portal.FileTransfer.RetrieveFiles: sandbox paths (/run/user/…/doc/…) for the drag's key
fn portal_files(key: &str) -> Result<Vec<PathBuf>, String> {
    use dbus::arg::PropMap;
    let c = dbus::blocking::Connection::new_session().map_err(|e| e.to_string())?;
    let px = c.with_proxy("org.freedesktop.portal.Documents", "/org/freedesktop/portal/documents", std::time::Duration::from_secs(10));
    let opts: PropMap = PropMap::new();
    let (v,): (Vec<String>,) = px.method_call("org.freedesktop.portal.FileTransfer", "RetrieveFiles", (key, opts)).map_err(|e| e.to_string())?;
    Ok(v.into_iter().map(PathBuf::from).collect())
}

/// text/uri-list: one URI per line, "#" = comment; only file:// (local files)
fn parse_uris(b: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    String::from_utf8_lossy(b)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        // file:///home/… or file://host/home/…
        .map(|rest| if rest.starts_with('/') { rest } else { rest.find('/').map_or("", |i| &rest[i..]) })
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(std::ffi::OsString::from_vec(unescape(p))))
        .collect()
}

/// %20 -> the byte (paths may not be UTF-8)
fn unescape(s: &str) -> Vec<u8> {
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uris() {
        let v = parse_uris(b"# c\r\nfile:///home/w/My%20Clip.mp4\r\nfile://host/tmp/a%25b.mkv\r\nhttps://x/y.mp4\r\nfile:///x%2\r\n");
        assert_eq!(v, vec![PathBuf::from("/home/w/My Clip.mp4"), PathBuf::from("/tmp/a%b.mkv"), PathBuf::from("/x%2")]);
    }
}
