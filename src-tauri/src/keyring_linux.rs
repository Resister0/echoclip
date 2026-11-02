//! Linux: Echo's secrets (Echo Cloud sign-in, R2 keys, the cloud config password) in the desktop's keyring
//! through the freedesktop Secret Service API - KDE Wallet on Plasma, GNOME Keyring on GNOME.
//! Items: attributes application=echo, name=<field>. When the keyring is unlocked at login (usual on
//! Nobara/KDE) nothing is ever shown; a locked keyring shows the desktop's own unlock dialog once.
use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
use dbus::blocking::Connection;
use dbus::message::MatchRule;
use dbus::Path;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEST: &str = "org.freedesktop.secrets";
const SVC: &str = "org.freedesktop.Secret.Service";
const T: Duration = Duration::from_secs(10);

/// Why a read didn't give a value.
#[derive(Debug, PartialEq)]
pub enum Miss {
    /// keyring works, the item isn't there
    NotFound,
    /// no keyring service / it couldn't be unlocked (try again later, don't throw anything away)
    Unavailable,
}

type Secret = (Path<'static>, Vec<u8>, Vec<u8>, String);

struct Ks {
    conn: Connection,
    session: Path<'static>,
}

fn open() -> Result<Ks, String> {
    let conn = Connection::new_session().map_err(|e| e.to_string())?;
    let p = conn.with_proxy(DEST, "/org/freedesktop/secrets", T);
    let (_, session): (Variant<Box<dyn RefArg>>, Path<'static>) =
        p.method_call(SVC, "OpenSession", ("plain", Variant(Box::new(String::new()) as Box<dyn RefArg>))).map_err(|e| e.to_string())?;
    Ok(Ks { conn, session })
}

impl Ks {
    fn svc(&self) -> dbus::blocking::Proxy<'_, &Connection> {
        self.conn.with_proxy(DEST, "/org/freedesktop/secrets", T)
    }

    /// Run a keyring prompt (unlock dialog) and wait until the user answers (max 2 minutes).
    fn prompt(&self, prompt: &Path) -> Result<(), String> {
        if &**prompt == "/" { return Ok(()); }
        let done = Arc::new(AtomicBool::new(false));
        let ok = Arc::new(AtomicBool::new(false));
        let (d2, o2) = (done.clone(), ok.clone());
        let rule = MatchRule::new_signal("org.freedesktop.Secret.Prompt", "Completed").with_path(prompt.clone().into_static());
        let tok = self.conn.add_match(rule, move |(dismissed, _): (bool, Variant<Box<dyn RefArg>>), _, _| {
            o2.store(!dismissed, Ordering::Relaxed);
            d2.store(true, Ordering::Relaxed);
            false
        }).map_err(|e| e.to_string())?;
        let pp = self.conn.with_proxy(DEST, prompt.clone(), T);
        let r: Result<(), _> = pp.method_call("org.freedesktop.Secret.Prompt", "Prompt", ("",));
        r.map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        while !done.load(Ordering::Relaxed) && t0.elapsed() < Duration::from_secs(120) {
            self.conn.process(Duration::from_millis(300)).map_err(|e| e.to_string())?;
        }
        let _ = self.conn.remove_match(tok);
        if ok.load(Ordering::Relaxed) { Ok(()) } else { Err("keyring stayed locked".into()) }
    }

    fn unlock(&self, objs: Vec<Path<'static>>) -> Result<(), String> {
        if objs.is_empty() { return Ok(()); }
        let (_, prompt): (Vec<Path<'static>>, Path<'static>) = self.svc().method_call(SVC, "Unlock", (objs,)).map_err(|e| e.to_string())?;
        self.prompt(&prompt)
    }

    /// The default collection ("kdewallet" on KDE, "login" on GNOME), unlocked. None = there is none
    /// (Echo doesn't create one: that would ask the user for a new password).
    fn collection(&self) -> Result<Option<Path<'static>>, String> {
        let (c,): (Path<'static>,) = self.svc().method_call(SVC, "ReadAlias", ("default",)).map_err(|e| e.to_string())?;
        if &*c == "/" { return Ok(None); }
        let locked: bool = self.conn.with_proxy(DEST, c.clone(), T).get("org.freedesktop.Secret.Collection", "Locked").unwrap_or(false);
        if locked { self.unlock(vec![c.clone()])?; }
        Ok(Some(c))
    }

    fn find(&self, name: &str) -> Result<Option<Path<'static>>, String> {
        let (unlocked, locked): (Vec<Path<'static>>, Vec<Path<'static>>) =
            self.svc().method_call(SVC, "SearchItems", (attrs(name),)).map_err(|e| e.to_string())?;
        if let Some(p) = unlocked.into_iter().next() { return Ok(Some(p)); }
        match locked.into_iter().next() {
            Some(p) => { self.unlock(vec![p.clone()])?; Ok(Some(p)) }
            None => Ok(None),
        }
    }
}

fn attrs(name: &str) -> HashMap<String, String> {
    HashMap::from([("application".to_string(), "echo".to_string()), ("name".to_string(), name.to_string())])
}

static UP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Is there a keyring with a default collection? (asked once per run)
pub fn available() -> bool {
    *UP.get_or_init(|| open().and_then(|k| k.collection()).map_or(false, |c| c.is_some()))
}

/// Save (or replace) one secret.
pub fn store(name: &str, value: &str) -> Result<(), String> {
    let k = open()?;
    let c = k.collection()?.ok_or("no default keyring")?;
    let mut props = PropMap::new();
    props.insert("org.freedesktop.Secret.Item.Label".into(), Variant(Box::new(format!("Echo ({name})")) as Box<dyn RefArg>));
    props.insert("org.freedesktop.Secret.Item.Attributes".into(), Variant(Box::new(attrs(name)) as Box<dyn RefArg>));
    let secret: Secret = (k.session.clone(), Vec::new(), value.as_bytes().to_vec(), "text/plain; charset=utf8".into());
    let (_, prompt): (Path<'static>, Path<'static>) = k.conn.with_proxy(DEST, c, T)
        .method_call("org.freedesktop.Secret.Collection", "CreateItem", (props, secret, true)).map_err(|e| e.to_string())?;
    k.prompt(&prompt)
}

/// Read one secret.
pub fn load(name: &str) -> Result<String, Miss> {
    let k = open().map_err(|_| Miss::Unavailable)?;
    let item = k.find(name).map_err(|_| Miss::Unavailable)?.ok_or(Miss::NotFound)?;
    let (s,): (Secret,) = k.conn.with_proxy(DEST, item, T)
        .method_call("org.freedesktop.Secret.Item", "GetSecret", (k.session.clone(),)).map_err(|_| Miss::Unavailable)?;
    String::from_utf8(s.2).map_err(|_| Miss::NotFound)
}

/// Remove one secret (signed out: nothing left behind).
pub fn delete(name: &str) {
    if let Ok(k) = open() {
        if let Ok(Some(item)) = k.find(name) {
            let r: Result<(Path<'static>,), _> = k.conn.with_proxy(DEST, item, T).method_call("org.freedesktop.Secret.Item", "Delete", ());
            if let Ok((p,)) = r { let _ = k.prompt(&p); }
        }
    }
}
