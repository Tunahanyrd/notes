//! `SessionManager._login` on the pure sign-in flow (`notes_login`): browser
//! sign-in, ms-app callback into this binary (`__notes-login-callback`),
//! Notes/Cloud token steps and registration, then `adopt` + `on_connected`.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notes_host::Val;
use notes_login::{Closed, LoginContext, LoginError, LoginStore};
use rs_store::storage::StoreCore;

use crate::sessions::{Login, Sessions};

struct Store(Arc<StoreCore>);

impl LoginStore for Store {
    fn client_device_id(&self) -> Result<Option<String>, LoginError> {
        match self.0.meta("client_device_id") {
            Ok(Some(Val::Str(s))) if !s.is_empty() => Ok(Some(s)),
            Ok(Some(v)) if v.truthy() => Err(LoginError::Failed("client_device_id is not a str".into())),
            Ok(_) => Ok(None),
            Err(e) => Err(LoginError::Failed(e.to_string())),
        }
    }
    fn set_client_device_id(&self, value: &str) -> Result<(), LoginError> {
        self.0.set_meta("client_device_id", &Val::s(value)).map_err(|e| LoginError::Failed(e.to_string()))
    }
}

struct ClosedEvent(Arc<Sessions>);

impl Closed for ClosedEvent {
    fn is_set(&self) -> bool {
        self.0.closed.is_set()
    }
    fn wait(&self, timeout: Duration) {
        self.0.closed.wait(timeout);
    }
}

/// The interactive sign-in used by the pure engine.
pub struct NativeLogin;

impl Login for NativeLogin {
    fn login(&self, sessions: &Arc<Sessions>) {
        let store = sessions.store();
        let root = crate::launcher::project_root();
        // Optional on-disk override of the embedded default profile
        // (notes_login::DEFAULT_LOGIN_PROFILE); no install ships one.
        let profile = root.join("login_profile.json");
        // An AppImage runs from a fresh mount point each launch: the handler
        // must name the stable image file ($APPIMAGE), not the mounted exe.
        let handler = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file())
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| PathBuf::from("notes-engine"));
        let ctx = LoginContext {
            store_directory: store.directory.clone(),
            session: sessions.path(),
            store: &Store(Arc::clone(&store)),
            layout: notes_login::Layout::from_env(),
            default_profile: profile.is_file().then_some(profile),
            closed: &ClosedEvent(Arc::clone(sessions)),
            net: &notes_login::net::NotesNet,
            runtime: &notes_login::SystemRuntime,
            handler_executable: handler,
        };
        let outcome = notes_login::login(&ctx).map_err(|e| e.user_message().to_string()).and_then(|session| {
            sessions.adopt_locked(&session).map(|_| session.clone()).map_err(|e| {
                if e.is_store() {
                    e.message
                } else {
                    notes_login::LOGIN_FAILED.to_string()
                }
            })
        });
        match outcome {
            Ok(session) => {
                sessions.on_connected();
                show_login_done(&session);
            }
            Err(message) => sessions.state.lock().unwrap().error = Some(message),
        }
    }
}

/// `SessionManager.LOGIN_DONE_PAGE` (exact bytes).
pub const LOGIN_DONE_PAGE: &str = include_str!("login_done_page.html");

/// `SessionManager._show_login_done(session)`: the browser tab stays on
/// Samsung's page after the app-scheme hand-off; open a local page saying the
/// sign-in finished. Best effort: never fails the login.
pub fn show_login_done(session: &std::path::Path) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = session.join("login-done.html");
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .and_then(|mut f| f.write_all(LOGIN_DONE_PAGE.as_bytes()));
    if written.is_err() {
        return;
    }
    let mut command = notes_platform::open::command();
    command.arg(&path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    notes_platform::process::detach_session(&mut command);
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}
