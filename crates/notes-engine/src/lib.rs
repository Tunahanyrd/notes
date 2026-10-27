//! Python-free Linux Notes engine.
//!
//! Serves the same loopback HTTP API as `python -m samsung_notes` (routes,
//! status codes, JSON bytes, headers, security checks, runtime file) over the
//! same SQLite library, using only the pure-Rust cores of the area crates:
//! `rs_engine::server_core` (HTTP), `rs_store::storage::StoreCore` (library
//! and outbox), `notes_host` (area glue), `notes_cloud` (Cloud
//! transactions) and `notes_preview` (page previews).
pub mod backend;
pub mod cloud;
pub mod cloud_impl;
pub mod engine;
pub mod launcher;
pub mod login_impl;
pub mod net_impl;
pub mod preview_impl;
pub mod previews;
pub mod server;
pub mod sessions;
pub mod stub_previews;
pub mod util;

use std::sync::Arc;

/// The Cloud implementation linked into this build.
pub fn cloud_factory() -> Arc<dyn cloud::CloudFactory> {
    Arc::new(cloud_impl::NativeCloud::default())
}

/// The preview implementation linked into this build.
pub fn preview_factory() -> Arc<dyn engine::PreviewFactory> {
    Arc::new(preview_impl::NativePreviews)
}

/// Hidden subcommands of any executable hosting the engine (the binary, or
/// the desktop shell in embedded mode): the ms-app desktop handler
/// (`__notes-login-callback <uri>`) and the isolated ink preparation child.
/// Call first in `main()`; `Some(code)` means exit with it.
pub fn run_hidden_subcommand() -> Option<i32> {
    let raw: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if let Some(code) = notes_login::run_callback_os(&raw) {
        return Some(code);
    }
    let args: Vec<String> = raw.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    notes_preview::run_child(&args)
}

/// Deliver an `ms-app://...` sign-in callback received by the host
/// application itself (macOS: Apple Event → Tauri `RunEvent::Opened`)
/// instead of through the Linux desktop handler process. Runs the same
/// `notes_login::dispatch` the `__notes-login-callback` subcommand runs: it
/// only writes into the pending transaction under the research directory,
/// which the engine's sign-in wait loop picks up. Returns the outcome
/// (`authorization_code_captured` / `authorization_error`) or the fixed
/// user-facing failure text; the URI is never logged.
pub fn deliver_login_callback(uri: &str) -> Result<String, String> {
    notes_login::dispatch(&notes_login::Layout::from_env(), &notes_login::SystemRuntime, uri)
        .map_err(|_| notes_login::CALLBACK_FAILED.to_string())
}

/// macOS: one stderr line per missing external tool with its Homebrew
/// install hint (Linux prints nothing, as before).
pub fn report_missing_tools() {
    if cfg!(target_os = "macos") {
        for line in notes_platform::tools::missing_report() {
            eprintln!("notes-engine: {line}");
        }
    }
}
