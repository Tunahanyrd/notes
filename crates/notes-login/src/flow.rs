//! Port of `tools/notes_login.py` `start` and `dispatch` (callback handler).
use crate::callback::{capture, StateVerifier};
use crate::decrypt::{prepare_signin, verify_saved_callback_state};
use crate::error::{Error, Result};
use crate::files::{ensure_state, load, load_json, mkdir_private, mkdir_private_exist_ok, path_text, private_write, save_json};
use crate::handler::{install_handler, HandlerExec};
use crate::http::{get_ok, HttpRequest, Network};
use crate::json::{eq_int, Value};
use crate::runtime::{token_hex, unix_seconds, utc_stamp, Layout, Runtime};
use crate::urls::{html_escape, urlsplit};
use std::path::{Path, PathBuf};
use std::process::Stdio;

pub const ENTRYPOINT: &str = "https://account.samsung.com/accounts/WINSDK/getEntryPoint";

fn transaction_dir(value: &Value) -> Result<PathBuf> {
    match value.get("transactionDirectory") {
        Some(Value::Str(s)) => Ok(PathBuf::from(s)),
        Some(_) => Err(Error::type_("expected str, bytes or os.PathLike object")),
        None => Err(Error::key("transactionDirectory")),
    }
}

fn parent_is(path: &Path, parent: &Path) -> bool {
    path.parent().is_some_and(|p| p == parent)
}

/// `notes_login.start(profile_path, app_id_path, open_browser)` with the
/// handler `Exec=` line chosen by `exec`. Returns the new transaction
/// directory. Nothing is printed (the Python output was discarded by
/// `SessionManager.run`).
pub fn start(
    layout: &Layout,
    net: &dyn Network,
    rt: &dyn Runtime,
    profile_path: &Path,
    app_id_path: &Path,
    open_browser: bool,
    exec: &HandlerExec,
) -> Result<PathBuf> {
    ensure_state(&layout.state)?;
    if !crate::handler::declared_by_package() {
        install_handler(layout, exec, rt)?;
    }
    let sessions = layout.sessions();
    mkdir_private_exist_ok(&sessions)?;
    let pending_path = layout.state.join("pending.json");
    if pending_path.exists() {
        let previous = load(&pending_path)?;
        let txn = transaction_dir(&previous)?;
        if !parent_is(&txn, &sessions) {
            return Err(Error::value("invalid previous transaction path"));
        }
        let metadata = load(&txn.join("transaction.json"))?;
        let expires = metadata.get("expiresUnixSeconds").ok_or_else(|| Error::key("expiresUnixSeconds"))?;
        let expires = expires.any_number().ok_or_else(|| Error::type_("invalid expiry type"))?;
        if rt.now() < expires && !txn.join(".callback-claim").exists() {
            return Err(Error::value("a live login transaction is already pending"));
        }
    }
    let name = format!("{}{}", utc_stamp(rt), token_hex(rt, 4));
    let work = sessions.join(format!("{name}-inputs"));
    mkdir_private(&work)?;
    let app_id_doc = load(app_id_path)?;
    let app_id = match app_id_doc.get("value") {
        Some(Value::Str(s)) if !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric()) => s.clone(),
        Some(_) => return Err(Error::value("invalid metadata application identity")),
        None => return Err(Error::key("value")),
    };
    let profile = load_json(profile_path, 32 * 1024)?;
    save_json(&work.join("profile.json"), &profile)?;
    use base64::Engine;
    let basic = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{app_id}:")));
    let request = HttpRequest::new(
        &format!("{ENTRYPOINT}?countryCode=TR"),
        None,
        &[
            ("x-osp-appId", app_id.as_str()),
            ("Authorization", basic.as_str()),
            ("Accept", "application/json"),
            ("User-Agent", "SamsungNotesLinuxResearch/0.1"),
        ],
        None,
    );
    let body = get_ok(net, &request, 15, 128 * 1024, "metadata endpoint did not return success")?;
    if body.len() > 128 * 1024 {
        return Err(Error::value("metadata exceeds size limit"));
    }
    private_write(&work.join("entrypoint.json"), &body)?;
    let directory = sessions.join(&name);
    prepare_signin(&work.join("entrypoint.json"), &work.join("profile.json"), &directory, rt)?;
    private_write(&directory.join("profile.json"), &std::fs::read(work.join("profile.json"))?)?;
    let pending = layout.state.join(format!("pending-{}.json", token_hex(rt, 4)));
    save_json(&pending, &Value::obj(vec![("transactionDirectory", Value::Str(path_text(&directory)?))]))?;
    std::fs::rename(&pending, &pending_path)?;
    let url = String::from_utf8(std::fs::read(directory.join("url.txt"))?).map_err(|_| Error::value("invalid url text"))?;
    // `login_landing(url)`: LANDING_TEMPLATE.replace('{{URL}}', html.escape(url, quote=True))
    let landing = LANDING_TEMPLATE.replace("{{URL}}", &html_escape(&url));
    let landing_path = directory.join("open-login.html");
    private_write(&landing_path, landing.as_bytes())?;
    if open_browser {
        open_detached(layout, &landing_path)?;
    }
    Ok(directory)
}

/// `notes_login.LANDING_TEMPLATE` (exact bytes).
pub const LANDING_TEMPLATE: &str = include_str!("login_landing.html");

/// `subprocess.Popen(['xdg-open', path], stdout=DEVNULL, stderr=DEVNULL,
/// start_new_session=True)`; the child is reaped in the background. The
/// opener is `notes_platform::open::OPENER` (`xdg-open` / macOS `open`).
fn open_detached(layout: &Layout, path: &Path) -> Result<()> {
    let mut cmd = layout.command(notes_platform::open::OPENER);
    cmd.arg(path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    notes_platform::process::detach_session(&mut cmd);
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn is_hex_state(s: &str) -> bool {
    (32..=32768).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `notes_login.dispatch(uri)`: the desktop handler. Returns the outcome
/// (`authorization_code_captured` / `authorization_error`).
pub fn dispatch(layout: &Layout, rt: &dyn Runtime, uri: &str) -> Result<String> {
    ensure_state(&layout.state)?;
    let pending = load(&layout.state.join("pending.json"))?;
    let directory = transaction_dir(&pending)?;
    let is_link = std::fs::symlink_metadata(&directory).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    if !parent_is(&directory, &layout.sessions()) || is_link {
        return Err(Error::value("invalid pending transaction path"));
    }
    save_json(
        &directory.join(format!("handler-invocation-{}.json", token_hex(rt, 4))),
        &Value::obj(vec![("invokedUnixSeconds", Value::Int(unix_seconds(rt)))]),
    )?;
    let verify = |ciphertext: &str, transaction: &Value| -> Result<bool> {
        if !eq_int(transaction.get("schemaVersion"), 3) || !is_hex_state(ciphertext) {
            return Ok(false);
        }
        Ok(verify_saved_callback_state(&directory, ciphertext).is_ok())
    };
    let verifier: StateVerifier<'_> = &verify;
    let outcome = match capture(&directory, uri, rt.now(), Some(verifier)) {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(reason) = error.callback_message() {
                let transaction = load(&directory.join("transaction.json"))?;
                let expected_text = match transaction.get("expectedCallback") {
                    Some(Value::Str(s)) => s.clone(),
                    Some(_) => return Err(Error::type_("expected callback must be a string")),
                    None => return Err(Error::key("expectedCallback")),
                };
                let expected = urlsplit(&expected_text).map_err(|_| Error::value("invalid expected callback"))?;
                let received = urlsplit(uri).map_err(|_| Error::value("invalid callback URI"))?;
                if uri.len() <= 65536 && (received.scheme.as_str(), received.hostname()) == (expected.scheme.as_str(), expected.hostname()) {
                    save_json(
                        &directory.join(format!("rejected-callback-{}.json", token_hex(rt, 4))),
                        &Value::obj(vec![
                            ("uri", Value::s(uri)),
                            ("reason", Value::s(reason)),
                            ("receivedUnixSeconds", Value::Int(unix_seconds(rt))),
                        ]),
                    )?;
                }
            }
            return Err(error);
        }
    };
    if layout.which("notify-send").is_some() {
        let message = if outcome == "authorization_code_captured" {
            "Giriş yanıtı alındı. İşleme terminalde devam ediliyor."
        } else {
            "Samsung giriş işlemi hata yanıtıyla tamamlandı."
        };
        let _ = layout
            .command("notify-send")
            .args(["Samsung Notes araştırması", message])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    Ok(outcome)
}
