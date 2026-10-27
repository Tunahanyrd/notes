//! Collaborators the store core needs from other areas (document, ink, page
//! operations) plus the store methods a caller may override. The Python
//! wrapper implements this by dispatching to Python; a pure-Rust host can
//! implement it with the other area crates once they are stable.
use crate::db::{Error, Result, Sql};
use crate::storage::StoreCore;
use crate::val::Val;

/// An exception raised by a collaborator, classified like the Python
/// `except` clauses that inspect it. `error` re-raises it unchanged.
#[derive(Debug)]
pub struct Fault {
    pub document: bool,
    pub ink: bool,
    pub value: bool,
    pub type_: bool,
    pub key: bool,
    pub overflow: bool,
    /// str(exception)
    pub message: String,
    pub error: Error,
}

impl Fault {
    pub fn other(error: Error) -> Fault {
        Fault { document: false, ink: false, value: false, type_: false, key: false, overflow: false, message: error.to_string(), error }
    }
}

impl From<Error> for Fault {
    fn from(e: Error) -> Self {
        let mut f = Fault::other(e);
        match &f.error {
            Error::Value(_) | Error::Decode(_) => f.value = true,
            Error::Type(_) => f.type_ = true,
            Error::Key(_) => f.key = true,
            Error::Overflow(_) => f.overflow = true,
            _ => {}
        }
        f
    }
}

pub type FaultResult<T> = std::result::Result<T, Fault>;

/// ink_coordinates.MIN_PEN_SIZE / MAX_PEN_SIZE
pub const MIN_PEN_SIZE: f64 = 0.5;
pub const MAX_PEN_SIZE: f64 = 128.0;
/// ink_highlighter.HIGHLIGHTER_RECIPE / HIGHLIGHTER_ALPHA / MIN_SIZE / MAX_SIZE
pub const HIGHLIGHTER_RECIPE: i64 = 5;
pub const HIGHLIGHTER_ALPHA: i64 = 0x73;
pub const HIGHLIGHTER_MIN_SIZE: f64 = 1.0;
pub const HIGHLIGHTER_MAX_SIZE: f64 = 128.0;

pub trait Host: Sync {
    /// A parsed `DocumentSnapshot` handed back to `prepare_ink_append`.
    type Snapshot;

    // ---- store methods a caller may override (Python: instance/subclass attribute)
    fn apply_document_tx(&self, store: &StoreCore, db: &dyn Sql, identity: &str, document: &Val, expected_job: Option<&Val>) -> Result<bool>
    where
        Self: Sized,
    {
        store.apply_document_tx(self, db, identity, document, expected_job)
    }
    fn apply_favorite_tx(&self, _store: &StoreCore, db: &dyn Sql, identity: &str, document: &Val) -> Result<()> {
        crate::storage::apply_favorite_tx(db, identity, document)
    }
    fn guard_ink_job(&self, _store: &StoreCore, db: &dyn Sql, job: &Val) -> Result<()> {
        crate::ink_store::guard_ink_job_tx(db, job)
    }
    fn ensure_no_active_ink(&self, db: &dyn Sql, identity: &str) -> Result<()> {
        crate::ink_store::ensure_no_active_ink(db, identity)
    }
    /// Store.organize delegates here (overridable organize_cloud).
    fn organize_cloud(&self, store: &StoreCore, identity: &str, changes: &Val) -> Result<Val>
    where
        Self: Sized,
    {
        store.organize_cloud(self, identity, changes)
    }
    /// Store.resolve re-applies snapshots through here (overridable apply_document).
    fn apply_document(&self, store: &StoreCore, server_id: &Val, document: &Val) -> Result<String>
    where
        Self: Sized,
    {
        store.apply_document(self, server_id, document, None)
    }

    // ---- other areas
    fn normalize_model(&self, model: &Val) -> Result<Val>;
    fn page_background_state(&self, document: Option<&Val>) -> Result<Val>;
    fn prepare_page_background(&self, document: &Val, color_argb: i64) -> FaultResult<()>;
    #[allow(clippy::too_many_arguments)]
    fn prepare_pdf_pages(&self, document: &Val, file_sha256: &Val, file_name: &Val, page_sizes: &Val, page_indices: &Val, template_type: &Val) -> FaultResult<()>;
    fn page_ops_state(&self, document: Option<&Val>) -> Result<Val>;
    fn prepare_add_page(&self, document: &Val, after_index: &Val) -> FaultResult<()>;
    fn prepare_delete_page(&self, document: &Val, page_ids: &Val) -> FaultResult<()>;
    fn prepare_reorder_pages(&self, document: &Val, page_ids: &Val) -> FaultResult<()>;
    /// storage.save_page_op op 'shape': shape_intent + prepare_add_shape dry
    /// run; returns the spec to store ({'pageId', 'shape': normalized}).
    fn prepare_add_shape(&self, document: &Val, spec: &Val) -> FaultResult<Val>;
    fn prepare_ink_delete(&self, document: &Val, page_id: &Val, layer_id: &Val, stroke_ids: &Val) -> FaultResult<()>;
    /// ink_erase.prepare_ink_move dry run (lasso move); dx/dy already exact int/float.
    #[allow(clippy::too_many_arguments)]
    fn prepare_ink_move(&self, document: &Val, page_id: &Val, layer_id: &Val, stroke_ids: &Val, dx: f64, dy: f64) -> FaultResult<()>;
    fn snapshot_from_dict(&self, document: &Val) -> Result<Self::Snapshot>;
    /// ink_first_stroke.prepare_first_stroke dry run (recipe 4).
    #[allow(clippy::too_many_arguments)]
    fn prepare_first_stroke(&self, snapshot: &Self::Snapshot, page_id: &str, points: &Val, color_argb: i64, size: f64, now_ms: i64, new_stroke_id: &str) -> FaultResult<()>;
    /// ink_highlighter.prepare_highlighter_stroke dry run (recipe 5).
    #[allow(clippy::too_many_arguments)]
    fn prepare_highlighter_stroke(&self, snapshot: &Self::Snapshot, page_id: &str, points: &Val, color_argb: i64, size: f64, now_ms: i64, new_stroke_id: &str) -> FaultResult<()>;
    /// prepare_ink_append_v2 (recipe 2) or _v3 (recipe 3) for one validated
    /// stroke; returns the prepared edit's page_id.
    fn prepare_ink_append(&self, snapshot: &Self::Snapshot, recipe: i64, stroke: &Val, now_ms: i64) -> FaultResult<Val>;
}
