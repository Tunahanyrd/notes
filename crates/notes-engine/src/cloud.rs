//! The Cloud boundary of the engine (`CloudAPI` / `CloudMetadata` /
//! `cloud_page_ops` / `cloud_ink_erase` as `Engine` uses them).
//!
//! The pure implementation lives in `crates/notes-cloud`; [`crate::cloud_impl`]
//! adapts it. Tests use fakes. Every method mirrors the Python call the
//! engine makes, returns the same dict (`Val`) and raises the same error
//! classes (`Fail` kinds Api/Conflict/Document/Os/...).
use std::path::Path;
use std::sync::Arc;

use notes_host::snapshot::Snapshot;
use notes_host::{Fail, Val};

use crate::util::R;

pub trait Cloud: Send + Sync {
    /// `api.auth_status()` -> `{'valid': ..., 'renewable': ..., ...}`
    fn auth_status(&self) -> R<Val>;
    /// `api.initial_cursor`
    fn initial_cursor(&self) -> Val;
    /// `api.changes(cursor)` -> `{'changes': [...], 'last_change_point': ..., 'has_next': ...}`
    fn changes(&self, cursor: &Val) -> R<Val>;
    /// `api.get_note(server_id)`
    fn get_note(&self, server_id: &Val) -> R<Snapshot>;
    /// `api._headers()` (preview downloads).
    fn headers(&self) -> R<Vec<(String, String)>>;
    fn create(&self, title: &Val, body: &Val, operation_id: &Val, local_id: &Val) -> R<Val>;
    fn create_editor(&self, title: &Val, model: &Val, operation_id: &Val, local_id: &Val) -> R<Val>;
    fn update(&self, server_id: &Val, snapshot: &Snapshot, title: &Val, body: &Val, operation_id: &Val) -> R<Val>;
    fn update_title(&self, server_id: &Val, snapshot: &Snapshot, title: &Val, operation_id: &Val) -> R<Val>;
    fn update_editor(&self, server_id: &Val, snapshot: &Snapshot, title: &Val, model: &Val, operation_id: &Val) -> R<Val>;
    fn update_page_background(&self, server_id: &Val, snapshot: &Snapshot, color_argb: &Val, operation_id: &Val) -> R<Val>;
    #[allow(clippy::too_many_arguments)]
    fn add_pdf_pages(
        &self,
        server_id: &Val,
        snapshot: &Snapshot,
        pdf: &[u8],
        file_name: &Val,
        page_sizes: &Val,
        page_indices: &Val,
        template_type: &Val,
        operation_id: &Val,
    ) -> R<Val>;
    /// `{2: api.append_ink_stroke_v2, 3: api.append_ink_stroke_v3}.get(recipe, api.append_ink_stroke)(
    ///     server_id, snapshot, source_stroke_id, samples=samples_from_points(points),
    ///     new_stroke_id=, operation_id=, **({'color_argb':, 'pen_size':} if recipe == 3))`
    #[allow(clippy::too_many_arguments)]
    fn append_ink_stroke(
        &self,
        recipe: i64,
        server_id: &Val,
        snapshot: &Snapshot,
        source_stroke_id: &Val,
        points: &Val,
        new_stroke_id: &Val,
        operation_id: &str,
        style: Option<(&Val, &Val)>,
    ) -> R<Val>;
    /// `cloud_page_ops.append_first_stroke(api, server_id, snapshot, page_id, samples, color_argb=, size=, new_stroke_id=, operation_id=)`
    #[allow(clippy::too_many_arguments)]
    fn append_first_stroke(
        &self,
        server_id: &Val,
        snapshot: &Snapshot,
        page_id: &Val,
        points: &Val,
        color_argb: &Val,
        size: &Val,
        new_stroke_id: &Val,
        operation_id: &str,
    ) -> R<Val>;
    /// `cloud_page_ops.append_highlighter_stroke(api, server_id, snapshot, page_id, samples, color_argb=, size=, new_stroke_id=, operation_id=)`
    #[allow(clippy::too_many_arguments)]
    fn append_highlighter_stroke(
        &self,
        _server_id: &Val,
        _snapshot: &Snapshot,
        _page_id: &Val,
        _points: &Val,
        _color_argb: &Val,
        _size: &Val,
        _new_stroke_id: &Val,
        _operation_id: &str,
    ) -> R<Val> {
        Err(Fail::api("cloud_unavailable", "Fosforlu kalem bu bulut bağlantısında desteklenmiyor."))
    }
    /// `cloud_ink_erase.erase_ink_strokes(api, server_id, snapshot, page_id, layer_id, stroke_ids, operation_id=)`
    fn erase_ink_strokes(&self, server_id: &Val, snapshot: &Snapshot, page_id: &Val, layer_id: &Val, stroke_ids: &Val, operation_id: &str) -> R<Val>;
    /// `cloud_ink_erase.move_ink_strokes(api, server_id, snapshot, page_id, layer_id, stroke_ids, dx, dy, operation_id=)`
    #[allow(clippy::too_many_arguments)]
    fn move_ink_strokes(
        &self,
        _server_id: &Val,
        _snapshot: &Snapshot,
        _page_id: &Val,
        _layer_id: &Val,
        _stroke_ids: &Val,
        _dx: &Val,
        _dy: &Val,
        _operation_id: &str,
    ) -> R<Val> {
        Err(Fail::api("cloud_unavailable", "Taşıma bu bulut bağlantısında desteklenmiyor."))
    }
    /// `cloud_shapes.add_shape(api, server_id, snapshot, page_id, shape, operation_id=)`
    fn add_shape(&self, _server_id: &Val, _snapshot: &Snapshot, _page_id: &Val, _shape: &Val, _operation_id: &Val) -> R<Val> {
        Err(Fail::api("cloud_unavailable", "Şekil bu bulut bağlantısında desteklenmiyor."))
    }
    /// `cloud_page_ops.add_page/delete_pages/reorder_pages(api, server_id, snapshot, <arg>, operation_id=)`
    fn page_op(&self, op: &str, server_id: &Val, snapshot: &Snapshot, argument: &Val, operation_id: &Val) -> R<Val>;
    /// `CloudMetadata(self)` for factories that build it from the concrete API.
    fn metadata(&self) -> Option<Arc<dyn Metadata>> {
        None
    }
}

/// `CloudMetadata(api)` as the engine uses it.
pub trait Metadata: Send + Sync {
    fn inventory(&self) -> R<Val>;
    fn folder_changes(&self, cursor: &Val) -> R<Val>;
    fn note_inventory(&self) -> R<Val>;
    fn update_organization(&self, server_id: &Val, snapshot: &Snapshot, changes: &Val, operation_id: &Val, baseline_node: &Val) -> R<Val>;
    fn create_folder(&self, name: &Val, parent_id: &Val, operation_id: &Val, folder_id: &Val) -> R<Val>;
    fn update_folder(&self, folder_id: &Val, changes: &Val, baseline_node: &Val, operation_id: &Val) -> R<Val>;
}

/// `api_factory(session_path, work_dir)` and `metadata_factory(api)`.
pub trait CloudFactory: Send + Sync {
    fn open(&self, session: &Path, work_dir: &Path) -> R<Arc<dyn Cloud>>;
    /// `None` = no metadata sync (Python `metadata_factory=None`).
    fn metadata(&self, api: &Arc<dyn Cloud>) -> Option<Arc<dyn Metadata>>;
}

/// Factory used while no pure Cloud implementation is linked: every Cloud
/// call fails like an unreachable service (the engine keeps local data).
pub struct Unavailable;

impl CloudFactory for Unavailable {
    fn open(&self, _session: &Path, _work_dir: &Path) -> R<Arc<dyn Cloud>> {
        Err(Fail::api("cloud_unavailable", "Samsung Cloud bağlantısı bu sürümde henüz kullanılamıyor."))
    }
    fn metadata(&self, _api: &Arc<dyn Cloud>) -> Option<Arc<dyn Metadata>> {
        None
    }
}
