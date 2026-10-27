//! The `ms-app` desktop scheme handler (`notes_login.install_handler`,
//! `desktop_quote`) with a Python-free `Exec=` variant.
//!
//! # Handler switch (Python → engine binary)
//!
//! The Python handler line is `Exec=<python3> <tools/notes_login.py> callback %u`.
//! The pure engine installs `Exec=<notes-engine> __notes-login-callback %u`
//! instead; every other line, the receipt (`handler-install.json`), the
//! mimeapps rollback snapshot and the `xdg-mime` / `update-desktop-database` /
//! `desktop-file-validate` calls are identical to Python.
//!
//! An *existing* Python installation (receipt present) is switched to the
//! binary only by [`install_handler`] with [`HandlerExec::Engine`], which runs
//! only when the user signs in through the pure engine (`login` → `start`).
//! The switch refuses unless the installed file is byte-for-byte our template
//! apart from its `Exec=` line (Python, or an engine binary that has moved), keeps a private backup
//! (`handler-before-switch-<hex>.desktop`) and writes a private
//! `handler-switch-<hex>.json` receipt; `handler-install.json` is left
//! unchanged, so `notes_login.py uninstall-handler` still works afterwards.
use crate::error::{Error, Result};
use crate::files::{ensure_state, load, path_text, private_write, save_json};
use crate::json::Value;
use crate::runtime::{token_hex, Layout, Runtime};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub const DESKTOP_NAME: &str = "samsung-notes-research-callback.desktop";
const MIME: &str = "x-scheme-handler/ms-app";
/// Hidden subcommand the engine binary dispatches before anything else.
pub const CALLBACK_SUBCOMMAND: &str = "__notes-login-callback";

/// True when the installed package already declares the `ms-app` handler,
/// so sign-in must not write a user desktop entry or call `xdg-mime`:
/// inside Flatpak (`/.flatpak-info`; the exported
/// `<app-id>.LoginCallback.desktop` runs `<app> __notes-login-callback %u`
/// through `flatpak run`, and neither the host's applications directory nor
/// `xdg-mime` is reachable from the sandbox) or with
/// `NOTES_URL_HANDLER=package` (distribution packages that ship the entry).
pub fn declared_by_package() -> bool {
    std::env::var("NOTES_URL_HANDLER").is_ok_and(|v| v == "package") || Path::new("/.flatpak-info").exists()
}

/// `notes_login.desktop_quote(value)`
pub fn desktop_quote(value: &str) -> Result<String> {
    if !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-')) {
        return Ok(value.to_string());
    }
    if value.contains('%') || value.contains('\n') || value.contains('\r') {
        return Err(Error::value("unsupported desktop executable path"));
    }
    let mut quoted = String::new();
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    Ok(format!("\"{}\"", quoted.replace('\\', "\\\\")))
}

/// What the desktop file's `Exec=` line runs.
#[derive(Clone, Debug)]
pub enum HandlerExec {
    /// `Exec=<interpreter> <script> callback %u` (the Python tool; kept for
    /// byte-for-byte comparison with `notes_login.install_handler`).
    Python { interpreter: PathBuf, script: PathBuf },
    /// `Exec=<notes-engine> __notes-login-callback %u`
    Engine { executable: PathBuf },
}

impl HandlerExec {
    fn exec_line(&self) -> Result<String> {
        Ok(match self {
            HandlerExec::Python { interpreter, script } => format!(
                "Exec={} {} callback %u\n",
                desktop_quote(&path_text(interpreter)?)?,
                desktop_quote(&path_text(script)?)?
            ),
            HandlerExec::Engine { executable } => {
                format!("Exec={} {} %u\n", desktop_quote(&path_text(executable)?)?, CALLBACK_SUBCOMMAND)
            }
        })
    }
}

const DESKTOP_HEAD: &str = "[Desktop Entry]\nType=Application\nName=Samsung Notes Research Login\n\
NoDisplay=true\nTerminal=false\nMimeType=x-scheme-handler/ms-app;\n";

/// The full desktop file text for an `Exec=` variant.
pub fn desktop_entry(exec: &HandlerExec) -> Result<String> {
    Ok(format!("{DESKTOP_HEAD}{}", exec.exec_line()?))
}

fn query_default(layout: &Layout) -> Result<String> {
    layout.run_command(&[OsStr::new("xdg-mime"), OsStr::new("query"), OsStr::new("default"), OsStr::new(MIME)])
}

fn validate_and_refresh(layout: &Layout, desktop: &Path, applications: &Path) -> Result<()> {
    if layout.which("desktop-file-validate").is_some() {
        layout.run_command(&[OsStr::new("desktop-file-validate"), desktop.as_os_str()])?;
    }
    if layout.which("update-desktop-database").is_some() {
        layout.run_command(&[OsStr::new("update-desktop-database"), applications.as_os_str()])?;
    }
    Ok(())
}

/// `notes_login.install_handler()` with a selectable `Exec=` line.
///
/// Only for `notes_platform::url_scheme::Registration::XdgDesktopEntry`
/// (Linux). On macOS the scheme is declared by the app bundle's
/// `Info.plist` and the callback arrives as an Apple Event in the running
/// app (`notes_engine::deliver_login_callback`), so nothing is installed and
/// no user configuration is touched.
pub fn install_handler(layout: &Layout, exec: &HandlerExec, rt: &dyn Runtime) -> Result<()> {
    ensure_state(&layout.state)?;
    if notes_platform::url_scheme::REGISTRATION == notes_platform::url_scheme::Registration::AppBundle {
        return Ok(());
    }
    let applications = layout.data_home.join("applications");
    std::fs::create_dir_all(&applications)?;
    let destination = applications.join(DESKTOP_NAME);
    let receipt = layout.state.join("handler-install.json");
    if receipt.exists() {
        if query_default(layout)? != DESKTOP_NAME {
            return Err(Error::value("callback handler changed since installation; inspect before replacing"));
        }
        if let HandlerExec::Engine { .. } = exec {
            switch_to(layout, exec, &receipt, &destination, &applications, rt)?;
        }
        return Ok(());
    }
    let old = query_default(layout)?;
    if !old.is_empty() || destination.exists() {
        return Err(Error::value("an existing ms-app handler or desktop file must be reviewed first"));
    }
    let config = layout.config_home.join("mimeapps.list");
    if config.exists() {
        private_write(&layout.state.join("mimeapps-before-install.list"), &std::fs::read(&config)?)?;
    }
    private_write(&destination, desktop_entry(exec)?.as_bytes())?;
    validate_and_refresh(layout, &destination, &applications)?;
    layout.run_command(&[OsStr::new("xdg-mime"), OsStr::new("default"), OsStr::new(DESKTOP_NAME), OsStr::new(MIME)])?;
    if query_default(layout)? != DESKTOP_NAME {
        return Err(Error::value("desktop handler registration could not be verified"));
    }
    save_json(
        &receipt,
        &Value::obj(vec![
            ("desktopFile", Value::Str(path_text(&destination)?)),
            ("previousDefault", Value::Str(old)),
            ("mimeappsFile", Value::Str(path_text(&config)?)),
            ("installed", Value::Bool(true)),
        ]),
    )?;
    Ok(())
}

/// True when `text` is our template with a Python (`... callback %u`) or an
/// earlier engine (`... __notes-login-callback %u`) Exec line.
fn is_known_template(text: &str) -> bool {
    let Some(exec) = text.strip_prefix(DESKTOP_HEAD) else { return false };
    let Some(line) = exec.strip_suffix('\n') else { return false };
    line.starts_with("Exec=")
        && (line.ends_with(" callback %u") || line.ends_with(&format!(" {CALLBACK_SUBCOMMAND} %u")))
        && !line.contains('\n')
        && !line.contains('\r')
}

fn switch_to(layout: &Layout, exec: &HandlerExec, receipt: &Path, destination: &Path, applications: &Path, rt: &dyn Runtime) -> Result<()> {
    let desired = desktop_entry(exec)?;
    let installed = load(receipt)?;
    if installed.get("desktopFile").and_then(Value::as_str) != Some(path_text(destination)?.as_str()) {
        return Err(Error::value("callback handler file changed since installation; inspect before replacing"));
    }
    let current = crate::files::read_limited(destination, 64 * 1024)?;
    if current == desired.as_bytes() {
        return Ok(());
    }
    let text = String::from_utf8(current.clone()).unwrap_or_default();
    if current.len() > 64 * 1024 || !is_known_template(&text) {
        return Err(Error::value("callback handler file changed since installation; inspect before replacing"));
    }
    let stamp = token_hex(rt, 4);
    let backup = layout.state.join(format!("handler-before-switch-{stamp}.desktop"));
    private_write(&backup, &current)?;
    let staged = applications.join(format!("samsung-notes-research-callback-switch-{stamp}.desktop"));
    private_write(&staged, desired.as_bytes())?;
    if layout.which("desktop-file-validate").is_some() {
        if let Err(e) = layout.run_command(&[OsStr::new("desktop-file-validate"), staged.as_os_str()]) {
            let _ = std::fs::remove_file(&staged);
            return Err(e);
        }
    }
    std::fs::rename(&staged, destination)?;
    if layout.which("update-desktop-database").is_some() {
        layout.run_command(&[OsStr::new("update-desktop-database"), applications.as_os_str()])?;
    }
    if query_default(layout)? != DESKTOP_NAME {
        return Err(Error::value("desktop handler registration could not be verified"));
    }
    save_json(
        &layout.state.join(format!("handler-switch-{stamp}.json")),
        &Value::obj(vec![
            ("desktopFile", Value::Str(path_text(destination)?)),
            ("previousFileBackup", Value::Str(path_text(&backup)?)),
            ("handler", Value::s("notes-engine")),
            ("switchedUnixSeconds", Value::Int(crate::runtime::unix_seconds(rt))),
        ]),
    )?;
    Ok(())
}
