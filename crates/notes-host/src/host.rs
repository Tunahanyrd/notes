//! `rs_store::host::Host` on the pure area cores (the Python-free twin of
//! `rs_store::pystore::PyHost`).
//!
//! Store methods keep their default (native) implementations; collaborators
//! call rs-ops / rs-ink / rs-editor directly through [`PureDeps`]. Collaborator
//! failures travel as [`Fail`] inside `rs_store::db::Error::External` (or as
//! `db::Error::Store` for a StoreError); faults are classified exactly like
//! the Python `except` clauses see the exception class.
use std::any::Any;

use rs_store::db::{Error, Result as R};
use rs_store::host::{Fault, FaultResult, Host};
use rs_store::storage::DRY_RUN_MS;
use rs_store::val::Val;

use crate::areas;
use crate::deps::{fail_of_ink, fail_of_ops, PureDeps};
use crate::err::{Fail, Kind};
use crate::snapshot::{self, Snapshot};

/// Classify a collaborator failure like `PyHost`'s `fault()` (isinstance
/// tests against DocumentError, InkError, ValueError, TypeError, KeyError,
/// OverflowError).
pub fn fault(f: Fail) -> Fault {
    Fault {
        document: f.is_document(),
        ink: f.kind == Kind::Ink,
        value: f.is_value(),
        type_: f.is_type(),
        key: f.is_key(),
        overflow: f.kind == Kind::Overflow,
        message: f.message.clone(),
        error: Error::from(f),
    }
}

fn ops_fault(e: rs_ops::err::Error) -> Fault {
    fault(fail_of_ops(e))
}

/// `type(v) is int` → value (saturated to the i128 range), like
/// `rs_ops::ink_erase::exact_int`.
pub fn exact_int(v: &Val) -> Option<i128> {
    match v {
        Val::Int(i) => Some(*i as i128),
        Val::BigInt(s) => Some(match s.trim().parse::<i128>() {
            Ok(i) => i,
            Err(_) if s.trim_start().starts_with('-') => i128::MIN,
            Err(_) => i128::MAX,
        }),
        _ => None,
    }
}

/// `obj.extract::<String>().ok()`
fn opt_str(v: &Val) -> Option<String> {
    v.as_str().map(String::from)
}

/// Python iteration of a JSON value (`None` = not iterable).
fn iter_items(v: &Val) -> Option<Vec<Val>> {
    match v {
        Val::List(items) => Some(items.clone()),
        Val::Dict(items) => Some(items.iter().map(|(k, _)| Val::s(k)).collect()),
        Val::Str(s) => Some(s.chars().map(|c| Val::s(c.to_string())).collect()),
        _ => None,
    }
}

fn not_iterable(v: &Val) -> Fail {
    Fail::type_(format!("'{}' object is not iterable", v.type_name()))
}

/// `float(x)` as PyO3's `f64` extraction sees a JSON value.
fn as_float(v: &Val) -> Result<f64, Fail> {
    match v {
        Val::Int(i) => Ok(*i as f64),
        Val::Bool(b) => Ok(*b as i64 as f64),
        Val::Float(f) => Ok(*f),
        Val::BigInt(s) => match s.trim().parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(f),
            _ => Err(Fail::new(Kind::Overflow, "int too large to convert to float")),
        },
        other => Err(Fail::type_(format!("must be real number, not {}", other.type_name()))),
    }
}

/// page_ops defers the TypeError of non-iterable ids until after its snapshot load.
fn not_iterable_after_load(deps: &PureDeps, document: &Val, page_ids: &Val) -> Fault {
    match rs_ops::page_ops::load(deps, document as &dyn Any, Some(DRY_RUN_MS as i128)) {
        Err(e) => ops_fault(e),
        Ok(_) => fault(not_iterable(page_ids)),
    }
}

/// The pure `Host`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PureHost {
    pub deps: PureDeps,
}

impl PureHost {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Host for PureHost {
    type Snapshot = Snapshot;

    fn normalize_model(&self, model: &Val) -> R<Val> {
        areas::normalize_model(model).map_err(Error::from)
    }

    fn page_background_state(&self, document: Option<&Val>) -> R<Val> {
        areas::page_background_state_with(&self.deps, document).map_err(Error::from)
    }

    fn prepare_page_background(&self, document: &Val, color_argb: i64) -> FaultResult<()> {
        rs_ops::page_style::prepare_page_background(&self.deps, document as &dyn Any, Some(color_argb as i128), Some(DRY_RUN_MS as i128))
            .map(|_| ())
            .map_err(ops_fault)
    }

    fn prepare_pdf_pages(&self, document: &Val, file_sha256: &Val, file_name: &Val, page_sizes: &Val, page_indices: &Val, template_type: &Val) -> FaultResult<()> {
        let mut sizes = Vec::new();
        for item in iter_items(page_sizes).ok_or_else(|| fault(not_iterable(page_sizes)))? {
            let pair = iter_items(&item).ok_or_else(|| fault(not_iterable(&item)))?;
            if pair.len() != 2 {
                return Err(fault(Fail::value(format!("expected 2 values to unpack (got {})", pair.len()))));
            }
            sizes.push((as_float(&pair[0]).map_err(fault)?, as_float(&pair[1]).map_err(fault)?));
        }
        let page_indices = match page_indices {
            Val::Null => None,
            other => Some(iter_items(other).ok_or_else(|| fault(not_iterable(other)))?.iter().map(exact_int).collect()),
        };
        let args = rs_ops::page_template::PdfArgs {
            file_sha256: opt_str(file_sha256),
            upload_token: Some(String::new()),
            file_name: opt_str(file_name),
            page_sizes: sizes,
            now_ms: Some(DRY_RUN_MS as i128),
            template_type: if template_type.is_null() { None } else { Some(exact_int(template_type)) },
            page_indices,
            ids: None,
        };
        rs_ops::page_template::prepare_pdf_pages(&self.deps, document as &dyn Any, &args).map(|_| ()).map_err(ops_fault)
    }

    fn page_ops_state(&self, document: Option<&Val>) -> R<Val> {
        areas::page_ops_state_with(&self.deps, document).map_err(Error::from)
    }

    fn prepare_add_page(&self, document: &Val, after_index: &Val) -> FaultResult<()> {
        rs_ops::page_ops::prepare_add_page(&self.deps, document as &dyn Any, exact_int(after_index), Some(DRY_RUN_MS as i128), None, None, None, None)
            .map(|_| ())
            .map_err(ops_fault)
    }

    fn prepare_add_shape(&self, document: &Val, spec: &Val) -> FaultResult<Val> {
        rs_store::shapes::dry_run(&self.deps, document as &dyn Any, spec).map_err(ops_fault)
    }

    fn prepare_delete_page(&self, document: &Val, page_ids: &Val) -> FaultResult<()> {
        let ids: Vec<Option<String>> = match page_ids {
            Val::Str(s) => vec![Some(s.clone())],
            Val::List(items) => items.iter().map(opt_str).collect(),
            Val::Dict(items) => items.iter().map(|(k, _)| Some(k.clone())).collect(),
            _ => return Err(not_iterable_after_load(&self.deps, document, page_ids)),
        };
        rs_ops::page_ops::prepare_delete_page(&self.deps, document as &dyn Any, ids, Some(DRY_RUN_MS as i128)).map(|_| ()).map_err(ops_fault)
    }

    fn prepare_reorder_pages(&self, document: &Val, page_ids: &Val) -> FaultResult<()> {
        let ids: Vec<Option<String>> = match page_ids {
            Val::Str(s) => s.chars().map(|c| Some(c.to_string())).collect(),
            Val::List(items) => items.iter().map(opt_str).collect(),
            Val::Dict(items) => items.iter().map(|(k, _)| Some(k.clone())).collect(),
            _ => return Err(not_iterable_after_load(&self.deps, document, page_ids)),
        };
        rs_ops::page_ops::prepare_reorder_pages(&self.deps, document as &dyn Any, ids, Some(DRY_RUN_MS as i128)).map(|_| ()).map_err(ops_fault)
    }

    fn prepare_ink_move(&self, document: &Val, page_id: &Val, layer_id: &Val, stroke_ids: &Val, dx: f64, dy: f64) -> FaultResult<()> {
        let (ids, lazy): (rs_ops::ink_erase::StrokeIds, Option<Fail>) = match stroke_ids {
            Val::Str(_) | Val::Bytes(_) => (None, None),
            other => match iter_items(other) {
                Some(items) => (Some(items.iter().map(opt_str).collect()), None),
                None => (Some(vec![]), Some(not_iterable(other))),
            },
        };
        let edit = rs_ops::ink_erase::prepare_ink_move(
            &self.deps,
            document as &dyn Any,
            opt_str(page_id).as_deref(),
            opt_str(layer_id).as_deref(),
            &ids,
            Some(dx),
            Some(dy),
            Some(DRY_RUN_MS as i128),
        );
        match (edit, lazy) {
            (Ok(_), _) => Ok(()),
            (Err(e), Some(err)) if e.kind == rs_ops::err::Kind::Ink && e.msg == "1-64 stroke identities required" => Err(fault(err)),
            (Err(e), _) => Err(ops_fault(e)),
        }
    }

    fn prepare_ink_delete(&self, document: &Val, page_id: &Val, layer_id: &Val, stroke_ids: &Val) -> FaultResult<()> {
        // ink_erase.stroke_ids_arg: str/bytes → None; other iterables → items;
        // a non-iterable raises its TypeError only where Python iterates it.
        let (ids, lazy): (rs_ops::ink_erase::StrokeIds, Option<Fail>) = match stroke_ids {
            Val::Str(_) | Val::Bytes(_) => (None, None),
            other => match iter_items(other) {
                Some(items) => (Some(items.iter().map(opt_str).collect()), None),
                None => (Some(vec![]), Some(not_iterable(other))),
            },
        };
        let edit = rs_ops::ink_erase::prepare_ink_delete(
            &self.deps,
            document as &dyn Any,
            opt_str(page_id).as_deref(),
            opt_str(layer_id).as_deref(),
            &ids,
            Some(DRY_RUN_MS as i128),
        );
        match (edit, lazy) {
            (Ok(_), _) => Ok(()),
            (Err(e), Some(err)) if e.kind == rs_ops::err::Kind::Ink && e.msg == "1-64 stroke identities required" => Err(fault(err)),
            (Err(e), _) => Err(ops_fault(e)),
        }
    }

    fn snapshot_from_dict(&self, document: &Val) -> R<Snapshot> {
        snapshot::from_dict(document).map_err(Error::from)
    }

    fn prepare_first_stroke(&self, snapshot: &Snapshot, page_id: &str, points: &Val, color_argb: i64, size: f64, now_ms: i64, new_stroke_id: &str) -> FaultResult<()> {
        let samples = crate::deps::raw_samples_from_points(points).map_err(fault)?;
        rs_ops::ink_first_stroke::prepare_first_stroke(
            &self.deps,
            snapshot as &dyn Any,
            Some(page_id),
            Some(&samples as &dyn Any),
            Some(color_argb as i128),
            Some(size),
            Some(now_ms as i128),
            Some(Some(new_stroke_id)),
        )
        .map(|_| ())
        .map_err(ops_fault)
    }

    fn prepare_highlighter_stroke(&self, snapshot: &Snapshot, page_id: &str, points: &Val, color_argb: i64, size: f64, now_ms: i64, new_stroke_id: &str) -> FaultResult<()> {
        let samples = crate::deps::raw_samples_from_points(points).map_err(fault)?;
        rs_ops::ink_highlighter::prepare_highlighter_stroke(
            &self.deps,
            snapshot as &dyn Any,
            Some(page_id),
            Some(&samples as &dyn Any),
            Some(color_argb as i128),
            Some(size),
            Some(now_ms as i128),
            Some(Some(new_stroke_id)),
        )
        .map(|_| ())
        .map_err(ops_fault)
    }

    fn prepare_ink_append(&self, snapshot: &Snapshot, recipe: i64, stroke: &Val, now_ms: i64) -> FaultResult<Val> {
        let append = rs_ink::core::prepare::AppendSnapshot::from_doc(snapshot);
        let mut xy = Vec::new();
        let mut pressures = Vec::new();
        let mut times = Vec::new();
        if let Some(Val::List(points)) = stroke.get("points") {
            for p in points {
                let f = |k: &str| p.get(k).and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
                xy.push((f("x"), f("y")));
                pressures.push(f("pressure"));
                times.push(match p.get("timeMs") {
                    Some(Val::Int(t)) => *t,
                    _ => return Err(fault(Fail::new(Kind::Overflow, "Python int too large to convert to C long"))),
                });
            }
        }
        let samples = rs_ink::core::binary::RawSamples::from_parts(&xy, &pressures, &times);
        let source = stroke.get_or_null("sourceStrokeId");
        let new_id = stroke.get_or_null("id");
        let (source, new_id) = (source.as_str().unwrap_or(""), new_id.as_str().unwrap_or(""));
        let limits = self.deps.limits;
        let edit = if recipe == 3 {
            let color = stroke.get_or_null("colorARGB").as_i64().unwrap_or(0) as u32;
            let size = stroke.get_or_null("size").as_f64().unwrap_or(f64::NAN);
            rs_ink::core::prepare::append_v3(&append, source, new_id, &samples, now_ms, color, size, Some(&limits))
        } else {
            rs_ink::core::prepare::append_v2(&append, source, new_id, &samples, now_ms, Some(&limits))
        }
        .map_err(|e| fault(fail_of_ink(e)))?;
        Ok(Val::Str(edit.page_id.clone()))
    }
}
