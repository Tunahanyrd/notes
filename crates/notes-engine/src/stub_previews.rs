//! Minimal preview managers used when no page renderer is linked: same
//! directories and error contract, no images.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notes_host::{Fail, Val};
use rs_store::storage::StoreCore;

use crate::engine::{ApiCache, PreviewFactory};
use crate::previews::{Previews, Thumbnails};
use crate::util::R;

pub struct NoPreviews {
    directory: PathBuf,
    store: Arc<StoreCore>,
}

impl Previews for NoPreviews {
    fn manifest(&self, identity: &str) -> R<Val> {
        self.store.raw(identity)?;
        Ok(Val::dict(vec![("pages", Val::List(vec![])), ("providerAvailable", Val::Bool(false))]))
    }
    fn request(&self, identity: &str, _page: i64, _priority: i64, _wait: f64) -> R<Option<Vec<u8>>> {
        self.store.raw(identity)?;
        Err(Fail::store("preview_unavailable", "Bu sayfa için görüntü önizlemesi bulunmuyor.", 404))
    }
    fn prefetch(&self) -> R<usize> {
        Ok(0)
    }
    fn close(&self) {}
    fn join(&self, _timeout: Duration) {}
    fn directory(&self) -> PathBuf {
        self.directory.clone()
    }
    fn template_pdf(&self, _document: &Val, _sha256: &str, _account: &str, _fetch: bool) -> R<Option<PathBuf>> {
        Err(Fail::store("template_unavailable", "Şablon dosyası bu sürümde okunamadı.", 422))
    }
}

pub struct NoThumbnails {
    store: Arc<StoreCore>,
}

impl Thumbnails for NoThumbnails {
    fn request(&self, identity: &str, _revision: i64) -> R<(Option<Vec<u8>>, bool)> {
        self.store.raw(identity)?;
        Err(Fail::store("thumbnail_unavailable", "Bu notun ilk sayfa görüntüsü bulunmuyor.", 404))
    }
}

pub struct NoPreviewFactory;

impl PreviewFactory for NoPreviewFactory {
    fn build(&self, store: &Arc<StoreCore>, _api: &Arc<ApiCache>) -> R<(Arc<dyn Previews>, Arc<dyn Thumbnails>)> {
        let directory = rs_store::storage::private_directory(&store.directory.join("page-previews"))?;
        let previews: Arc<dyn Previews> = Arc::new(NoPreviews { directory, store: Arc::clone(store) });
        rs_store::storage::private_directory(&store.directory.join("thumbnails"))?;
        Ok((previews, Arc::new(NoThumbnails { store: Arc::clone(store) })))
    }
}
