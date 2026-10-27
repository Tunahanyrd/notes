//! `SessionManager._login` without Python: every former subprocess
//! (`notes_login.py start`, `notes-cli decrypt-callback`, the three
//! `notes_auth_bridge.py` steps, `notes_cloud.py register`) runs in-process.
use crate::bridge::{authenticate, authorize_notes, exchange_notes_code};
use crate::cloud::register_session;
use crate::decrypt::decrypt_saved_callback;
use crate::error::Error;
use crate::files::{load, private_directory, save_json};
use crate::flow::start;
use crate::handler::HandlerExec;
use crate::http::Network;
use crate::json::{self, Value};
use crate::runtime::{token_hex, Layout, Runtime};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The text `SessionManager._login` stores for every non-StoreError failure.
pub const LOGIN_FAILED: &str = "Giriş tamamlanamadı. Tek kullanımlık kod tekrar gönderilmedi; yeniden giriş yapabilirsin.";

/// The default login profile (`src/login_profile.json`, a copy of the
/// historical `samsung_notes/login_profile.json`), embedded so no install
/// needs the file (it is parsed and re-dumped exactly like Python did).
pub const DEFAULT_LOGIN_PROFILE: &[u8] = include_bytes!("login_profile.json");

/// The metadata (entry point) application identity `_login` writes.
pub const METADATA_CLIENT: &str = "a4558shhl3";

/// Why `login` failed. `user_message()` is what `SessionManager.error` shows.
#[derive(Debug)]
pub enum LoginError {
    /// `StoreError(code, message, status)` raised by the store; its message
    /// is shown verbatim (`except StoreError as error: self.error = error.message`).
    Store { code: String, message: String, status: u16 },
    /// Any other failure (`OSError`, `ValueError`, `KeyError`, `TypeError`,
    /// failed step). The detail is a private, secret-free diagnostic.
    Failed(String),
}

impl LoginError {
    pub fn user_message(&self) -> &str {
        match self {
            LoginError::Store { message, .. } => message,
            LoginError::Failed(_) => LOGIN_FAILED,
        }
    }
}

impl fmt::Display for LoginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoginError::Store { code, message, .. } => write!(f, "{code}: {message}"),
            LoginError::Failed(detail) => write!(f, "login failed: {detail}"),
        }
    }
}

impl std::error::Error for LoginError {}

impl From<Error> for LoginError {
    fn from(e: Error) -> LoginError {
        LoginError::Failed(e.to_string())
    }
}

fn failed(step: &str) -> LoginError {
    LoginError::Failed(format!("{step} failed; private research receipts retained"))
}

/// The store metadata `_login` reads/writes (`store.meta('client_device_id')`,
/// `store.set_meta('client_device_id', ...)`).
pub trait LoginStore: Send + Sync {
    /// The stored value when it is a non-empty string (`meta(...) or ...`);
    /// `None` when absent or falsy. A truthy non-string is an error
    /// (`'PCID:' + identity` raises TypeError in Python).
    fn client_device_id(&self) -> Result<Option<String>, LoginError>;
    fn set_client_device_id(&self, value: &str) -> Result<(), LoginError>;
}

/// `self.closed` (a `threading.Event`): `is_set()` and `wait(timeout)`.
pub trait Closed: Send + Sync {
    fn is_set(&self) -> bool;
    /// Block up to `timeout` or until set (default: sleep).
    fn wait(&self, timeout: Duration) {
        std::thread::sleep(timeout);
    }
}

/// Everything `SessionManager._login` used from `self`, the environment and
/// the subprocesses it started.
pub struct LoginContext<'a> {
    /// `self.store.directory` (the private application directory).
    pub store_directory: PathBuf,
    /// `self.path`: an adopted session whose `profile.json` is reused.
    pub session: Option<PathBuf>,
    pub store: &'a dyn LoginStore,
    /// `RESEARCH`/`STATE`, XDG data/config homes, helper-process environment.
    pub layout: Layout,
    /// `ROOT / 'samsung_notes/login_profile.json'`; `None` → embedded copy.
    pub default_profile: Option<PathBuf>,
    pub closed: &'a dyn Closed,
    pub net: &'a dyn Network,
    pub runtime: &'a dyn Runtime,
    /// The engine binary the `ms-app` desktop handler runs
    /// (`Exec=<path> __notes-login-callback %u`).
    pub handler_executable: PathBuf,
}

/// `SessionManager._login()` up to (not including) `self.adopt(session)`.
///
/// Returns the completed session directory (`RESEARCH/sessions/<name>` with
/// `cloud/cloud-token.json`); the caller adopts it under its lock and calls
/// `on_connected`. Never retries a single-use request.
pub fn login(ctx: &LoginContext<'_>) -> Result<PathBuf, LoginError> {
    let rt = ctx.runtime;
    let inputs = ctx.store_directory.join(format!("login-{}", token_hex(rt, 6)));
    private_directory(&inputs)?;
    let mut profile = match (&ctx.session, &ctx.default_profile) {
        (Some(path), _) => load(&path.join("profile.json"))?,
        (None, Some(path)) => load(path)?,
        (None, None) => json::loads(DEFAULT_LOGIN_PROFILE).map_err(Error::from)?,
    };
    if ctx.session.is_none() {
        let identity = match ctx.store.client_device_id()? {
            Some(id) => id,
            None => token_hex(rt, 16),
        };
        ctx.store.set_client_device_id(&identity)?;
        if !profile.is_object() {
            return Err(LoginError::Failed("login profile is not an object".into()));
        }
        profile.set("devicePhysicalAddressText", Value::Str(identity.clone()));
        profile.set("deviceUniqueID", Value::Str(format!("PCID:{identity}")));
    }
    save_json(&inputs.join("profile.json"), &profile).map_err(Error::from)?;
    save_json(&inputs.join("metadata-client.json"), &Value::obj(vec![("value", Value::s(METADATA_CLIENT))])).map_err(Error::from)?;

    let exec = HandlerExec::Engine { executable: ctx.handler_executable.clone() };
    start(&ctx.layout, ctx.net, rt, &inputs.join("profile.json"), &inputs.join("metadata-client.json"), true, &exec)
        .map_err(|_| failed("notes_login start"))?;

    let pending = load(&ctx.layout.state.join("pending.json"))?;
    let session = match pending.get("transactionDirectory") {
        Some(Value::Str(s)) => PathBuf::from(s),
        Some(_) => return Err(LoginError::Failed("invalid pending transaction".into())),
        None => return Err(LoginError::Failed("missing pending transaction".into())),
    };
    if session.parent() != Some(ctx.layout.sessions().as_path()) {
        return Err(LoginError::Failed("invalid login session location".into()));
    }
    let deadline = load(&session.join("transaction.json"))?
        .get("expiresUnixSeconds")
        .and_then(Value::any_number)
        .ok_or_else(|| LoginError::Failed("invalid transaction expiry".into()))?;
    wait_for_callback(&session, deadline, ctx.closed, rt)?;
    let outcome = load(&session.join("callback.json"))?;
    if outcome.get("outcome").and_then(Value::as_str) != Some("authorization_code_captured") {
        if outcome.get("outcome").is_none() {
            return Err(LoginError::Failed("callback has no outcome".into()));
        }
        return Err(LoginError::Failed("login did not return an authorization code".into()));
    }
    decrypt_saved_callback(&session).map_err(|_| failed("decrypt-callback"))?;
    type Step = fn(&dyn Network, &dyn Runtime, &Path) -> crate::error::Result<bool>;
    let steps: [(&str, Step); 3] =
        [("authenticate", authenticate), ("authorize-notes", authorize_notes), ("exchange-notes-code", exchange_notes_code)];
    for (name, step) in steps {
        if !matches!(step(ctx.net, rt, &session), Ok(true)) {
            return Err(failed(name));
        }
    }
    if !register_session(ctx.net, rt, &session, &profile)? {
        return Err(failed("notes_cloud register"));
    }
    Ok(session)
}

/// `while time.time() < deadline and not closed.is_set(): ... wait(0.5)`
fn wait_for_callback(session: &Path, deadline: f64, closed: &dyn Closed, rt: &dyn Runtime) -> Result<(), LoginError> {
    while rt.now() < deadline && !closed.is_set() {
        if session.join("callback.json").exists() {
            return Ok(());
        }
        closed.wait(Duration::from_millis(500));
    }
    Err(LoginError::Failed("login expired or application closed".into()))
}

/// Convenience: [`login`] → `Ok(session)` or the `SessionManager.error` text.
pub fn login_message(ctx: &LoginContext<'_>) -> Result<PathBuf, String> {
    login(ctx).map_err(|e| e.user_message().to_string())
}
