//! How the Samsung sign-in callback (`ms-app://...`) reaches the app.
//!
//! * Linux: a hidden XDG desktop entry
//!   (`samsung-notes-research-callback.desktop`, `x-scheme-handler/ms-app`)
//!   registered with `xdg-mime`; the desktop runs
//!   `<engine> __notes-login-callback <uri>` as a separate process
//!   (`notes_login::handler`).
//! * macOS: the `.app` declares the scheme in `Info.plist`
//!   (`CFBundleURLTypes`, see `desktop/src-tauri/Info.plist`); LaunchServices
//!   delivers the URL to the running app as an Apple Event, Tauri surfaces it
//!   as `RunEvent::Opened`, and the shell hands it to
//!   `notes_engine::deliver_login_callback` in-process. Nothing is written to
//!   the user's configuration at sign-in time.
pub const SCHEME: &str = "ms-app";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registration {
    /// XDG desktop entry + `xdg-mime default` (runtime install).
    XdgDesktopEntry,
    /// Declared by the application bundle (build time); nothing to install.
    AppBundle,
}

#[cfg(not(target_os = "macos"))]
pub const REGISTRATION: Registration = Registration::XdgDesktopEntry;
#[cfg(target_os = "macos")]
pub const REGISTRATION: Registration = Registration::AppBundle;
