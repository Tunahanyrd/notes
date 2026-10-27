//! Page preview / thumbnail boundary (`engine.previews`, `engine.thumbnails`).
//! The pure implementation is `crates/notes-preview`; see `preview_impl`.
use std::path::PathBuf;
use std::time::Duration;

use notes_host::Val;

use crate::util::R;

pub trait Previews: Send + Sync {
    /// `previews.manifest(identity)`
    fn manifest(&self, identity: &str) -> R<Val>;
    /// `previews.request(identity, page, priority=, wait=)`
    fn request(&self, identity: &str, page: i64, priority: i64, wait: f64) -> R<Option<Vec<u8>>>;
    /// `previews.prefetch()`
    fn prefetch(&self) -> R<usize>;
    fn close(&self);
    fn join(&self, timeout: Duration);
    /// `previews.directory`
    fn directory(&self) -> PathBuf;
    /// `apply_template`'s PDF lookup for the template `sha256` in the source
    /// note snapshot dict: Ok(Some(path)) when a cached/fetched copy exists,
    /// Ok(None) when it is not cached and `fetch` is false.
    /// Errors: `Fail::store('template_unavailable', ...)` codes exactly as engine.py.
    fn template_pdf(&self, document: &Val, sha256: &str, account: &str, fetch: bool) -> R<Option<PathBuf>>;
}

pub trait Thumbnails: Send + Sync {
    /// `thumbnails.request(identity, revision)` -> (png | None, complete)
    fn request(&self, identity: &str, revision: i64) -> R<(Option<Vec<u8>>, bool)>;
}
