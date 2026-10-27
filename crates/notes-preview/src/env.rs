//! What the preview code needs from the engine.
//!
//! The Python manager reads `engine.sessions.path`, `engine.sessions.offline`,
//! `engine._api()._headers()` and uses module-level transports (`previews._http`,
//! `fetch_pdf`'s download). Here those are one trait; the network client is
//! implemented by the engine (no HTTP code lives in this crate).
use notes_host::{Fail, Val};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Message of `previews._http` when the transport fails (OSError/URLError).
pub const DOWNLOAD_FAILED: &str = "Sayfa görüntüsü indirilemedi. Bağlantını kontrol edip tekrar deneyebilirsin.";

pub trait PreviewEnv: Send + Sync {
    /// `engine.sessions.path` (None when signed out).
    fn session_path(&self) -> Option<PathBuf>;

    /// `bool(engine.sessions.offline)`
    fn offline(&self) -> bool;

    /// `engine._api()._headers()` (unfiltered; callers drop Content-Type and
    /// Content-Encoding). Errors keep their class (APIError, StoreError, ...).
    fn headers(&self) -> Result<Vec<(String, String)>, Fail>;

    /// `previews._http(url, headers)` -> `(status, body, Location)`.
    ///
    /// Contract of the Python transport the implementation must keep: only
    /// global addresses (getaddrinfo answers checked), TLS with the default
    /// context, no redirects followed, 20 s timeout, body read only for 200
    /// and bounded to `MAX_DOWNLOAD` (larger -> `PreviewError('Bu sayfa
    /// önizleme boyutu sınırını aşıyor.')`), network failures ->
    /// `PreviewError(DOWNLOAD_FAILED)`.
    ///
    /// Default: no network in this build.
    fn http(&self, _url: &str, _headers: &[(String, String)]) -> Result<(u16, Vec<u8>, Option<String>), Fail> {
        Err(crate::util::preview(DOWNLOAD_FAILED))
    }

    /// `fetch_pdf`'s transport `(url, headers, temporary, bound, check) ->
    /// (status, Location)`: for 200 stream the body into `dest` (created with
    /// O_EXCL|O_NOFOLLOW, 0600), calling `check()` before every 64 KiB chunk,
    /// fail with `PdfBackgroundError('PDF önizleme boyutu sınırını aşıyor.')`
    /// beyond `max_bytes`, fsync. Network failures are OS errors (`Kind::Os`).
    ///
    /// Default: [`PreviewEnv::http`] plus the bounded private file write.
    fn download(&self, url: &str, headers: &[(String, String)], dest: &Path, max_bytes: u64,
                check: &dyn Fn() -> Result<(), Fail>) -> Result<(u16, Option<String>), Fail> {
        let (status, body, location) = self.http(url, headers)?;
        if status != 200 {
            return Ok((status, location));
        }
        write_bounded(dest, &body, max_bytes, check)?;
        Ok((200, None))
    }

    /// The source checkout / install root (`ROOT` of the Python modules):
    /// locates `tools/ink-renderer/target/*/ink-render` and the optional
    /// preview codec defaults. Default: `$LINUX_NOTES_PROJECT_ROOT`, else the
    /// checkout this crate was built from.
    fn project_root(&self) -> PathBuf {
        default_project_root()
    }

    /// Executable for the isolated ink preparation child (self-exec). `None`
    /// prepares in-process on a background thread (no rlimits). An engine
    /// binary whose `main()` first calls [`crate::ink::run_child`] returns
    /// `std::env::current_exe().ok()` here.
    fn ink_child_executable(&self) -> Option<PathBuf> {
        None
    }

    /// `_fetch_pdf_refreshing`'s note refresh: `row = store.raw(identity);
    /// fresh = engine._api().get_note(row['server_id']);
    /// store.apply_document(row['server_id'], fresh.to_dict())`, returning
    /// `fresh.to_dict()`. Default: no Cloud in this environment (the
    /// `_api()` failure Python raises when signed out).
    fn refresh_note(&self, _identity: &str) -> Result<Val, Fail> {
        Err(Fail::store("not_connected", "Samsung hesabına giriş yap. Yerel notların korunuyor.", 401))
    }
}

pub fn default_project_root() -> PathBuf {
    if let Some(root) = std::env::var_os("LINUX_NOTES_PROJECT_ROOT") {
        return PathBuf::from(root);
    }
    // crates/notes-preview -> repository root
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(|| manifest.to_path_buf())
}

/// The streaming write of `fetch_pdf`'s default download.
pub fn write_bounded(dest: &Path, body: &[u8], max_bytes: u64, check: &dyn Fn() -> Result<(), Fail>) -> Result<(), Fail> {
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW).open(dest)?;
    let mut total: u64 = 0;
    for chunk in body.chunks(65536) {
        check()?;
        total += chunk.len() as u64;
        if total > max_bytes {
            return Err(crate::util::pdf("PDF önizleme boyutu sınırını aşıyor."));
        }
        file.write_all(chunk)?;
    }
    check()?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}
