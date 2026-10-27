//! `session.SessionManager`: adopt an existing private login session, track
//! the login state shown in `/api/status`, disconnect.
//!
//! The browser login itself (`start_login` -> `_login`) and the post-login
//! registration are owned by the auth port (crates/rs-auth); they plug in
//! through [`Login`]. Until then [`NoLogin`] reports the same failure text
//! the Python flow shows when a login step fails.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use notes_host::{Fail, Val};
use rs_store::storage::StoreCore;

use crate::util::{meta_or, Event, R};

/// `session.RESEARCH` (`$XDG_STATE_HOME/samsung-notes-research`).
pub fn research_dir() -> PathBuf {
    rs_engine::server::research_dir()
}

/// `notes_login.load_json(path, limit=128 KiB)`
pub fn load_json(path: &Path) -> R<Val> {
    use std::io::Read;
    let mut raw = Vec::new();
    std::fs::File::open(path)?.take(128 * 1024 + 1).read_to_end(&mut raw)?;
    if raw.len() > 128 * 1024 {
        return Err(Fail::value("private input exceeds size limit"));
    }
    let text = String::from_utf8(raw).map_err(|_| Fail::value("invalid utf-8 in private input"))?;
    rs_store::val::loads(&text).map_err(|e| Fail::value(format!("{:?}", e)))
}

#[derive(Default, Clone)]
pub struct SessionState {
    pub path: Option<PathBuf>,
    pub pending: bool,
    pub error: Option<String>,
}

/// The interactive sign-in (`SessionManager._login`). Implementations run in
/// their own thread, call `sessions.adopt` on success and set the error text.
pub trait Login: Send + Sync {
    fn login(&self, sessions: &Arc<Sessions>);
}

/// Placeholder until the pure auth flow is STABLE.
pub struct NoLogin;

impl Login for NoLogin {
    fn login(&self, sessions: &Arc<Sessions>) {
        let mut s = sessions.state.lock().unwrap();
        s.error = Some("Giriş tamamlanamadı. Tek kullanımlık kod tekrar gönderilmedi; yeniden giriş yapabilirsin.".into());
        s.pending = false;
    }
}

pub struct Sessions {
    store: Arc<StoreCore>,
    offline: AtomicBool,
    pub state: Mutex<SessionState>,
    /// `self.lock`
    lock: Mutex<()>,
    pub closed: Event,
    on_connected: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    login: Arc<dyn Login>,
    research: PathBuf,
}

impl Sessions {
    /// `SessionManager(store, offline=, session=)`
    pub fn new(store: Arc<StoreCore>, offline: bool, session: Option<&Path>, login: Arc<dyn Login>) -> R<Arc<Sessions>> {
        Sessions::with_research(store, offline, session, login, research_dir())
    }

    /// `SessionManager` with an explicit `RESEARCH` directory (tests patch it).
    pub fn with_research(store: Arc<StoreCore>, offline: bool, session: Option<&Path>, login: Arc<dyn Login>, research: PathBuf) -> R<Arc<Sessions>> {
        let sessions = Arc::new(Sessions {
            store,
            offline: AtomicBool::new(offline),
            state: Mutex::new(SessionState::default()),
            lock: Mutex::new(()),
            closed: Event::new(),
            on_connected: Mutex::new(None),
            login,
            research,
        });
        if !offline && !meta_or(&sessions.store, "disconnected", Val::Bool(false))?.truthy() {
            let mut selected: Option<PathBuf> = match session {
                Some(p) => Some(p.to_path_buf()),
                None => match sessions.store.meta("session_path")? {
                    Some(v) if v.truthy() => match v {
                        Val::Str(s) => Some(PathBuf::from(s)),
                        _ => return Err(Fail::type_("expected str, bytes or os.PathLike object")),
                    },
                    _ => None,
                },
            };
            let pending = sessions.research.join("pending.json");
            if selected.is_none() && pending.exists() {
                let value = load_json(&pending)?;
                if let Some(Val::Str(s)) = value.get("transactionDirectory") {
                    if !s.is_empty() {
                        selected = Some(PathBuf::from(s));
                    }
                }
            }
            if let Some(path) = selected {
                if let Err(e) = sessions.adopt(&path) {
                    if e.is_os_value_key_type() {
                        sessions.state.lock().unwrap().error = Some("Oturum yüklenemedi. Samsung hesabına yeniden giriş yapabilirsin.".into());
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Ok(sessions)
    }

    pub fn set_on_connected(&self, f: Box<dyn Fn() + Send + Sync>) {
        *self.on_connected.lock().unwrap() = Some(f);
    }

    pub fn on_connected(&self) {
        if let Some(f) = self.on_connected.lock().unwrap().as_ref() {
            f();
        }
    }

    /// `self.offline`
    pub fn offline(&self) -> bool {
        self.offline.load(Ordering::SeqCst)
    }
    pub fn set_offline(&self, value: bool) {
        self.offline.store(value, Ordering::SeqCst)
    }
    /// `self.path = value` (tests and the login flow).
    pub fn set_path(&self, value: Option<PathBuf>) {
        self.state.lock().unwrap().path = value;
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.state.lock().unwrap().path.clone()
    }
    pub fn pending(&self) -> bool {
        self.state.lock().unwrap().pending
    }
    pub fn error(&self) -> Option<String> {
        self.state.lock().unwrap().error.clone()
    }

    /// `SessionManager.adopt(path)`
    pub fn adopt(&self, path: &Path) -> R<()> {
        use std::os::unix::fs::MetadataExt;
        let path = rs_engine::server_core::realpath(path);
        let sessions = rs_engine::server_core::realpath(&self.research.join("sessions"));
        if path.parent() != Some(sessions.as_path()) || !path.join("cloud/cloud-token.json").is_file() {
            return Err(Fail::value("only completed owned research sessions can be adopted"));
        }
        let meta = std::fs::metadata(&path)?;
        // SAFETY: getuid takes no arguments, has no preconditions and cannot fail.
        if meta.uid() != unsafe { libc::getuid() } || meta.mode() & 0o077 != 0 {
            return Err(Fail::value("session directory is not private"));
        }
        let token = load_json(&path.join("notes-account-token.json"))?;
        let client = crate::util::key_str(&token, "clientId")?.to_string();
        let user = crate::util::key_str(crate::util::key(&token, "response")?, "userId")?.to_string();
        let account = rs_engine::engine_core::sha256_hex(format!("{}:{}", client, user).as_bytes());
        if let Some(previous) = self.store.meta("account_key")? {
            if previous.truthy() && !matches!(&previous, Val::Str(p) if *p == account) {
                return Err(Fail::store(
                    "different_account",
                    "Bu yerel arşiv başka bir Samsung hesabına ait. Mevcut notlar o hesaba gönderilmedi.",
                    409,
                ));
            }
        }
        self.store.set_meta("account_key", &Val::s(account))?;
        self.store.set_meta("session_path", &Val::s(path.to_string_lossy()))?;
        self.store.set_meta("disconnected", &Val::Bool(false))?;
        let mut s = self.state.lock().unwrap();
        s.path = Some(path);
        s.error = None;
        Ok(())
    }

    /// `with self.lock: self.adopt(session)` (end of `_login`).
    pub fn adopt_locked(&self, path: &Path) -> R<()> {
        let _guard = self.lock.lock().unwrap();
        self.adopt(path)
    }

    pub fn store(&self) -> Arc<StoreCore> {
        Arc::clone(&self.store)
    }

    /// `SessionManager.disconnect()`
    pub fn disconnect(&self) -> R<Val> {
        let _guard = self.lock.lock().unwrap();
        if self.pending() {
            return Err(Fail::store("login_pending", "Devam eden girişi tarayıcıda iptal et veya tamamlanmasını bekle.", 409));
        }
        self.state.lock().unwrap().path = None;
        self.store.set_meta("disconnected", &Val::Bool(true))?;
        Ok(Val::dict(vec![("connected", Val::Bool(false))]))
    }

    /// `SessionManager.start_login()`
    pub fn start_login(self: &Arc<Self>) -> R<Val> {
        if self.offline() {
            return Err(Fail::store("offline_mode", "Bu oturum çevrimdışı başlatıldı. Normal başlatıcıyla yeniden aç.", 409));
        }
        {
            let _guard = self.lock.lock().unwrap();
            let mut s = self.state.lock().unwrap();
            if s.pending {
                return Ok(Val::Bool(false));
            }
            s.pending = true;
            s.error = None;
        }
        let me = Arc::clone(self);
        let login = Arc::clone(&self.login);
        std::thread::Builder::new()
            .name("notes-login".into())
            .spawn(move || {
                login.login(&me);
                me.state.lock().unwrap().pending = false;
            })
            .map_err(Fail::from)?;
        Ok(Val::Bool(true))
    }
}
