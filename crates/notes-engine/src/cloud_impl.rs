//! Adapter: `notes_cloud::{CloudApi, CloudMetadata, ops}` behind the engine's
//! `Cloud` / `Metadata` / `CloudFactory` traits. Argument values arrive as the
//! Python objects engine.py passes (`Val`); type mismatches raise the
//! TypeError-class failure the Python call would.
use std::path::Path;
use std::sync::Arc;

use notes_cloud::{CloudApi, CloudMetadata, Options};
use notes_host::snapshot::Snapshot;
use notes_host::{Fail, Val};

use crate::cloud::{Cloud, CloudFactory, Metadata};
use crate::util::R;

fn st(v: &Val) -> R<&str> {
    v.as_str().ok_or_else(|| Fail::type_(format!("expected str, got {}", v.type_name())))
}

fn opt_st(v: &Val) -> R<Option<&str>> {
    match v {
        Val::Null => Ok(None),
        other => st(other).map(Some),
    }
}

fn int(v: &Val) -> R<i64> {
    match v {
        Val::Int(i) => Ok(*i),
        Val::Bool(b) => Ok(*b as i64),
        other => Err(Fail::type_(format!("expected int, got {}", other.type_name()))),
    }
}

fn strings(v: &Val) -> R<Vec<String>> {
    match v {
        Val::List(items) => items.iter().map(|i| st(i).map(str::to_string)).collect(),
        Val::Str(s) => Ok(s.chars().map(|c| c.to_string()).collect()),
        other => Err(Fail::type_(format!("'{}' object is not iterable", other.type_name()))),
    }
}

/// Builds the transport/renewal/clock options for each `CloudApi`.
pub type OptionsFn = Arc<dyn Fn() -> Options + Send + Sync>;

/// `api_factory=CloudAPI`, `metadata_factory=CloudMetadata`.
pub struct NativeCloud {
    pub options: OptionsFn,
}

impl Default for NativeCloud {
    fn default() -> Self {
        NativeCloud { options: Arc::new(default_options) }
    }
}

/// Overrides the default `send_once` transport (tests, offline builds).
static TRANSPORT: std::sync::OnceLock<Arc<dyn notes_cloud::Transport>> = std::sync::OnceLock::new();

pub fn set_transport(transport: Arc<dyn notes_cloud::Transport>) {
    let _ = TRANSPORT.set(transport);
}

/// `CloudAPI(session, work_dir)` options: the installed transport (default:
/// the pure-Rust HTTPS `send_once`), ported renewal, system clock.
pub fn default_options() -> Options {
    let mut options = Options::default();
    options.transport = match TRANSPORT.get() {
        Some(t) => Arc::clone(t),
        None => Arc::new(crate::net_impl::SendOnce),
    };
    options
}

struct Api(Arc<CloudApi>);

impl Cloud for Api {
    fn auth_status(&self) -> R<Val> {
        self.0.auth_status()
    }
    fn initial_cursor(&self) -> Val {
        Val::s(self.0.initial_cursor.clone())
    }
    fn changes(&self, cursor: &Val) -> R<Val> {
        self.0.changes(opt_st(cursor)?)
    }
    fn get_note(&self, server_id: &Val) -> R<Snapshot> {
        self.0.get_note(st(server_id)?)
    }
    fn headers(&self) -> R<Vec<(String, String)>> {
        self.0.headers(false)
    }
    fn create(&self, title: &Val, body: &Val, operation_id: &Val, local_id: &Val) -> R<Val> {
        self.0.create(st(title)?, st(body)?, st(operation_id)?, opt_st(local_id)?)
    }
    fn create_editor(&self, title: &Val, model: &Val, operation_id: &Val, local_id: &Val) -> R<Val> {
        self.0.create_editor(st(title)?, model, st(operation_id)?, opt_st(local_id)?)
    }
    fn update(&self, server_id: &Val, snapshot: &Snapshot, title: &Val, body: &Val, operation_id: &Val) -> R<Val> {
        self.0.update(st(server_id)?, snapshot, st(title)?, st(body)?, None, st(operation_id)?)
    }
    fn update_title(&self, server_id: &Val, snapshot: &Snapshot, title: &Val, operation_id: &Val) -> R<Val> {
        self.0.update_title(st(server_id)?, snapshot, st(title)?, st(operation_id)?)
    }
    fn update_editor(&self, server_id: &Val, snapshot: &Snapshot, title: &Val, model: &Val, operation_id: &Val) -> R<Val> {
        self.0.update_editor(st(server_id)?, snapshot, st(title)?, model, None, st(operation_id)?)
    }
    fn update_page_background(&self, server_id: &Val, snapshot: &Snapshot, color_argb: &Val, operation_id: &Val) -> R<Val> {
        self.0.update_page_background(st(server_id)?, snapshot, int(color_argb)?, st(operation_id)?, None)
    }
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
    ) -> R<Val> {
        let sizes: Vec<(f64, f64)> = match page_sizes {
            Val::List(items) => items
                .iter()
                .map(|p| match p {
                    Val::List(wh) if wh.len() == 2 => Ok((wh[0].as_f64().unwrap_or(f64::NAN), wh[1].as_f64().unwrap_or(f64::NAN))),
                    _ => Err(Fail::value("expected 2 values to unpack")),
                })
                .collect::<R<_>>()?,
            other => return Err(Fail::type_(format!("'{}' object is not iterable", other.type_name()))),
        };
        let indices: Option<Vec<i64>> = match page_indices {
            Val::Null => None,
            Val::List(items) => Some(items.iter().map(int).collect::<R<_>>()?),
            other => return Err(Fail::type_(format!("'{}' object is not iterable", other.type_name()))),
        };
        let template = match template_type {
            Val::Null => None,
            other => Some(int(other)?),
        };
        self.0.add_pdf_pages(st(server_id)?, snapshot, pdf, st(file_name)?, &sizes, indices.as_deref(), template, st(operation_id)?, None)
    }
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
    ) -> R<Val> {
        let samples = notes_cloud::samples_from_points(points)?;
        let (server, source, new) = (st(server_id)?, st(source_stroke_id)?, st(new_stroke_id)?);
        match (recipe, style) {
            (2, _) => self.0.append_ink_stroke_v2(server, snapshot, source, &samples, new, operation_id, None),
            (3, Some((color, size))) => self.0.append_ink_stroke_v3(server, snapshot, source, &samples, new, operation_id, color, size, None),
            _ => self.0.append_ink_stroke(server, snapshot, source, &samples, new, operation_id, None),
        }
    }
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
    ) -> R<Val> {
        let samples = notes_cloud::samples_from_points(points)?;
        notes_cloud::ops::append_first_stroke(&self.0, st(server_id)?, snapshot, st(page_id)?, &samples, color_argb, size, st(new_stroke_id)?, operation_id, None)
    }
    fn append_highlighter_stroke(
        &self,
        server_id: &Val,
        snapshot: &Snapshot,
        page_id: &Val,
        points: &Val,
        color_argb: &Val,
        size: &Val,
        new_stroke_id: &Val,
        operation_id: &str,
    ) -> R<Val> {
        let samples = notes_cloud::samples_from_points(points)?;
        notes_cloud::ops::append_highlighter_stroke(&self.0, st(server_id)?, snapshot, st(page_id)?, &samples, color_argb, size, st(new_stroke_id)?, operation_id, None)
    }
    fn erase_ink_strokes(&self, server_id: &Val, snapshot: &Snapshot, page_id: &Val, layer_id: &Val, stroke_ids: &Val, operation_id: &str) -> R<Val> {
        notes_cloud::ops::erase_ink_strokes(&self.0, st(server_id)?, snapshot, st(page_id)?, st(layer_id)?, &strings(stroke_ids)?, operation_id, None)
    }
    fn move_ink_strokes(&self, server_id: &Val, snapshot: &Snapshot, page_id: &Val, layer_id: &Val, stroke_ids: &Val, dx: &Val, dy: &Val, operation_id: &str) -> R<Val> {
        notes_cloud::ops::move_ink_strokes(&self.0, st(server_id)?, snapshot, st(page_id)?, st(layer_id)?, &strings(stroke_ids)?, dx, dy, operation_id, None)
    }
    fn add_shape(&self, server_id: &Val, snapshot: &Snapshot, page_id: &Val, shape: &Val, operation_id: &Val) -> R<Val> {
        notes_cloud::ops::add_shape(&self.0, st(server_id)?, snapshot, st(page_id)?, shape, st(operation_id)?, None)
    }
    fn page_op(&self, op: &str, server_id: &Val, snapshot: &Snapshot, argument: &Val, operation_id: &Val) -> R<Val> {
        let (server, operation) = (st(server_id)?, st(operation_id)?);
        match op {
            "add" => notes_cloud::ops::add_page(&self.0, server, snapshot, int(argument)?, operation, None),
            "delete" => notes_cloud::ops::delete_pages(&self.0, server, snapshot, &strings(argument)?, operation, None),
            _ => notes_cloud::ops::reorder_pages(&self.0, server, snapshot, &strings(argument)?, operation, None),
        }
    }
    fn metadata(&self) -> Option<Arc<dyn Metadata>> {
        Some(Arc::new(Meta(CloudMetadata::new(Arc::clone(&self.0)))))
    }
}

struct Meta(CloudMetadata);

impl Metadata for Meta {
    fn inventory(&self) -> R<Val> {
        self.0.inventory()
    }
    fn folder_changes(&self, cursor: &Val) -> R<Val> {
        self.0.folder_changes(opt_st(cursor)?)
    }
    fn note_inventory(&self) -> R<Val> {
        self.0.note_inventory(256)
    }
    fn update_organization(&self, server_id: &Val, snapshot: &Snapshot, changes: &Val, operation_id: &Val, baseline_node: &Val) -> R<Val> {
        self.0.update_organization(st(server_id)?, snapshot, changes, st(operation_id)?, baseline_node)
    }
    fn create_folder(&self, name: &Val, parent_id: &Val, operation_id: &Val, folder_id: &Val) -> R<Val> {
        self.0.create_folder(st(name)?, st(parent_id)?, st(operation_id)?, opt_st(folder_id)?)
    }
    fn update_folder(&self, folder_id: &Val, changes: &Val, baseline_node: &Val, operation_id: &Val) -> R<Val> {
        self.0.update_folder(st(folder_id)?, changes, baseline_node, st(operation_id)?)
    }
}

impl CloudFactory for NativeCloud {
    fn open(&self, session: &Path, work_dir: &Path) -> R<Arc<dyn Cloud>> {
        Ok(Arc::new(Api(Arc::new(CloudApi::new(session, work_dir, (self.options)())?))))
    }
    fn metadata(&self, api: &Arc<dyn Cloud>) -> Option<Arc<dyn Metadata>> {
        api.metadata()
    }
}
