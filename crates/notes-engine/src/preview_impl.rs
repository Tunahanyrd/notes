//! Adapter: `notes_preview::{PreviewManager, ThumbnailManager}` behind the
//! engine's `Previews` / `Thumbnails` traits, with the engine's session and
//! `_api()` as the preview environment.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use notes_host::Val;
use notes_preview::{PreviewEnv, PreviewManager, ThumbnailManager};
use rs_store::storage::StoreCore;

use crate::engine::{ApiCache, PreviewFactory};
use crate::previews::{Previews, Thumbnails};
use crate::util::R;

/// Set by the `notes-engine` binary (whose `main()` dispatches
/// `notes_preview::run_child`): ink preparation then runs in an isolated
/// self-exec child like the Python `ink_render_job` child. Embedders (Tauri)
/// leave it unset and prepare on a background thread.
pub static SELF_EXEC_INK_CHILD: AtomicBool = AtomicBool::new(false);

struct Env {
    api: Arc<ApiCache>,
    manager: Mutex<Weak<PreviewManager>>,
}

impl PreviewEnv for Env {
    fn session_path(&self) -> Option<PathBuf> {
        self.api.sessions.path()
    }
    fn offline(&self) -> bool {
        self.api.sessions.offline()
    }
    fn headers(&self) -> R<Vec<(String, String)>> {
        self.api.api()?.headers()
    }
    fn http(&self, url: &str, headers: &[(String, String)]) -> R<(u16, Vec<u8>, Option<String>)> {
        crate::net_impl::preview_http(url, headers)
    }
    fn download(&self, url: &str, headers: &[(String, String)], dest: &std::path::Path, max_bytes: u64, check: &dyn Fn() -> R<()>) -> R<(u16, Option<String>)> {
        crate::net_impl::pdf_download(url, headers, dest, max_bytes, check)
    }
    fn refresh_note(&self, identity: &str) -> R<Val> {
        let store = self.api.sessions.store();
        let row = store.raw(identity)?;
        let server_id = crate::util::key(&row, "server_id")?.clone();
        let fresh = notes_host::snapshot::to_dict(&self.api.api()?.get_note(&server_id)?);
        store.apply_document(&notes_host::host::PureHost::new(), &server_id, &fresh, None)?;
        if let Some(manager) = self.manager.lock().unwrap().upgrade() {
            if let Err(error) = manager.wake_inventory() {
                eprintln!("notes-engine: preview inventory scheduling failed: {error}");
            }
        }
        Ok(fresh)
    }
    fn ink_child_executable(&self) -> Option<PathBuf> {
        if SELF_EXEC_INK_CHILD.load(Ordering::SeqCst) {
            std::env::current_exe().ok()
        } else {
            None
        }
    }
}

struct Native(Arc<PreviewManager>);

impl Previews for Native {
    fn snapshot_changed(&self) -> R<()> {
        self.0.wake_inventory()
    }
    fn retry_inventory(&self, identity: &str) -> R<Val> {
        self.0.retry_inventory(identity)
    }
    fn manifest(&self, identity: &str) -> R<Val> {
        self.0.manifest(identity)
    }
    fn request(&self, identity: &str, page: i64, priority: i64, wait: f64) -> R<Option<Vec<u8>>> {
        self.0.request(identity, page, priority, wait)
    }
    fn notext(&self, identity: &str, page: i64, revision: i64) -> R<Option<Vec<u8>>> {
        self.0.notext(identity, page, revision)
    }
    fn fulltext(&self, identity: &str, page: i64, revision: i64) -> R<Option<Vec<u8>>> {
        self.0.fulltext(identity, page, revision)
    }
    fn text_ink_layer(&self, identity:&str, page:i64, revision:i64, tile:Option<(u8,u32,u32)>, expected:Option<&str>) -> R<Option<(Vec<u8>,String)>> {
        self.0.text_ink_layer(identity,page,revision,tile,expected)
    }
    fn ink_layer(&self, identity: &str, page: i64, revision: i64, tile: Option<(u8,u32,u32)>, expected: Option<&str>) -> R<Option<(Vec<u8>,String)>> {
        self.0.ink_layer(identity, page, revision, tile, expected)
    }
    fn prefetch(&self) -> R<usize> {
        self.0.prefetch(notes_preview::MAX_QUEUED / 2)
    }
    fn close(&self) {
        self.0.close()
    }
    fn join(&self, timeout: Duration) {
        self.0.join(timeout)
    }
    fn directory(&self) -> PathBuf {
        self.0.directory().to_path_buf()
    }
    fn template_pdf(&self, document: &Val, sha256: &str, account: &str, fetch: bool) -> R<Option<PathBuf>> {
        self.0.template_pdf(document, sha256, account, fetch)
    }
}

struct Thumbs(ThumbnailManager);

impl Thumbnails for Thumbs {
    fn request(&self, identity: &str, revision: i64) -> R<(Option<Vec<u8>>, bool)> {
        self.0.request(identity, revision)
    }
}

pub struct NativePreviews;

impl PreviewFactory for NativePreviews {
    fn build(&self, store: &Arc<StoreCore>, api: &Arc<ApiCache>) -> R<(Arc<dyn Previews>, Arc<dyn Thumbnails>)> {
        let env = Arc::new(Env { api: Arc::clone(api), manager: Mutex::new(Weak::new()) });
        let manager = Arc::new(PreviewManager::new(Arc::clone(store), env.clone())?);
        *env.manager.lock().unwrap() = Arc::downgrade(&manager);
        let thumbnails = ThumbnailManager::new(Arc::clone(store), manager.clone())?;
        Ok((Arc::new(Native(manager)), Arc::new(Thumbs(thumbnails))))
    }
}
