//! Thin PyO3 wrappers: the `Store` class (Store + OrganizationStore +
//! EditorStore + InkStore mixins in one class), module functions, and the
//! `PyHost` that dispatches overridable methods and other-area collaborators
//! back to Python.
use crate::db::{self, Error, Result as R, Sql};
use crate::host::{Fault, FaultResult, Host};
use crate::pyconv::*;
use crate::storage::{RowGet, StoreCore, DRY_RUN_MS};
use crate::val::Val;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyString, PyTuple, PyType};
#[cfg(feature = "python")]
use _ops::bridge::{PyDeps as OpsDeps, RPy};
use std::path::PathBuf;

#[cfg(feature = "python")]
fn ok<T>(py: Python<'_>, r: R<T>) -> PyResult<T> {
    r.map_err(|e| err_to_py(py, e))
}

#[cfg(feature = "python")]
fn out(py: Python<'_>, r: R<Val>) -> PyResult<Py<PyAny>> {
    to_py(py, &ok(py, r)?)
}

#[cfg(feature = "python")]
fn v(obj: &Bound<'_, PyAny>) -> PyResult<Val> {
    to_val(obj)
}

#[cfg(feature = "python")]
fn path_obj(py: Python<'_>, p: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    Ok(py.import("pathlib")?.getattr("Path")?.call1((p,))?.unbind())
}

// ---------------------------------------------------------------- rs-editor values

fn to_editor(v: &Val) -> _editor::pyval::Val {
    use _editor::pyval::{parse_py_int, PyInt, Val as E};
    match v {
        Val::Null => E::None,
        Val::Bool(b) => E::Bool(*b),
        Val::Int(i) => E::Int(PyInt::from_i128(*i as i128), true),
        Val::BigInt(s) => E::Int(parse_py_int(s).unwrap_or_else(|| PyInt(s.clone())), true),
        Val::Float(f) => E::Float(*f),
        Val::Str(s) => E::Str(s.clone()),
        Val::Bytes(_) => E::Other(None),
        Val::List(items) => E::List(items.iter().map(to_editor).collect()),
        Val::Dict(items) => E::Dict(items.iter().map(|(k, v)| (E::Str(k.clone()), to_editor(v))).collect()),
    }
}

fn from_editor(v: &_editor::pyval::Val) -> R<Val> {
    use _editor::pyval::Val as E;
    Ok(match v {
        E::None => Val::Null,
        E::Bool(b) => Val::Bool(*b),
        E::Int(i, _) => match i.to_i64() {
            Some(x) => Val::Int(x),
            None => Val::BigInt(i.0.clone()),
        },
        E::Float(f) => Val::Float(*f),
        E::Str(s) => Val::Str(s.clone()),
        E::List(items) => Val::List(items.iter().map(from_editor).collect::<R<_>>()?),
        E::Dict(items) => Val::Dict(
            items
                .iter()
                .map(|(k, v)| Ok((k.as_str().map(String::from).ok_or_else(|| Error::Type("non-str key".into()))?, from_editor(v)?)))
                .collect::<R<_>>()?,
        ),
        E::BadStr(_) | E::Other(_) => return Err(Error::Type("unsupported value from rs-editor".into())),
    })
}

#[cfg(feature = "python")]
/// page_ops defers the TypeError of non-iterable ids until after its snapshot load.
fn not_iterable_after_load(py: Python<'_>, document: &Val, page_ids: &Val) -> PyResult<()> {
    let doc = to_py(py, document)?;
    _ops::page_ops::load(&OpsDeps { py }, &doc, Some(DRY_RUN_MS as i128)).py(py)?;
    let name = to_py(py, page_ids)?.bind(py).get_type().name()?.to_string();
    Err(pyo3::exceptions::PyTypeError::new_err(format!("'{}' object is not iterable", name)))
}

// ---------------------------------------------------------------- host

#[cfg(feature = "python")]
pub struct PyHost {
    obj: Py<PyAny>,
}

#[cfg(feature = "python")]
impl PyHost {
    #[cfg(feature = "python")]
    fn new(obj: &Bound<'_, PyAny>) -> PyHost {
        PyHost { obj: obj.clone().unbind() }
    }

    #[cfg(feature = "python")]
    /// True when a test/subclass replaced `name` (instance attribute or a
    /// subclass definition); the Rust implementation is used otherwise.
    fn overridden(&self, py: Python<'_>, name: &str) -> bool {
        let obj = self.obj.bind(py);
        if let Ok(d) = obj.getattr("__dict__") {
            if let Ok(d) = d.cast::<PyDict>() {
                if d.contains(name).unwrap_or(false) {
                    return true;
                }
            }
        }
        let ty = obj.get_type();
        let base = py.get_type::<Store>();
        if ty.is(&base) {
            return false;
        }
        match (ty.getattr(name), base.getattr(name)) {
            (Ok(a), Ok(b)) => !a.is(&b),
            _ => false,
        }
    }

    #[cfg(feature = "python")]
    fn call<'py, A: pyo3::call::PyCallArgs<'py>>(&self, py: Python<'py>, name: &str, args: A, kwargs: Option<&Bound<'py, PyDict>>) -> R<Bound<'py, PyAny>> {
        self.obj.bind(py).call_method(name, args, kwargs).map_err(py_err)
    }
}

#[cfg(feature = "python")]
fn pyv(py: Python<'_>, v: &Val) -> R<Py<PyAny>> {
    to_py(py, v).map_err(py_err)
}

#[cfg(feature = "python")]
fn fault(py: Python<'_>, e: PyErr) -> Fault {
    let value = e.value(py);
    let is = |module: &str, name: &str| -> bool {
        py.import(module)
            .and_then(|m| m.getattr(name))
            .and_then(|t| value.is_instance(&t))
            .unwrap_or(false)
    };
    Fault {
        document: is("samsung_notes.document", "DocumentError"),
        ink: is("samsung_notes.ink", "InkError"),
        value: is("builtins", "ValueError"),
        type_: is("builtins", "TypeError"),
        key: is("builtins", "KeyError"),
        overflow: is("builtins", "OverflowError"),
        message: value.str().map(|s| s.to_string()).unwrap_or_default(),
        error: py_err(e),
    }
}

#[cfg(feature = "python")]
fn faulting_val(f: impl FnOnce(Python<'_>) -> PyResult<Val>) -> FaultResult<Val> {
    Python::attach(|py| f(py).map_err(|e| fault(py, e)))
}

#[cfg(feature = "python")]
/// Call a collaborator that may raise a classified fault.
fn faulting(f: impl FnOnce(Python<'_>) -> PyResult<()>) -> FaultResult<()> {
    Python::attach(|py| f(py).map_err(|e| fault(py, e)))
}

#[cfg(feature = "python")]
impl Host for PyHost {
    #[cfg(feature = "python")]
    type Snapshot = Py<PyAny>;

    #[cfg(feature = "python")]
    fn apply_document_tx(&self, store: &StoreCore, db: &dyn Sql, identity: &str, document: &Val, expected_job: Option<&Val>, prepared: Option<&crate::storage::PreparedSnapshot>) -> R<bool> {
        let dispatch = Python::attach(|py| self.overridden(py, "_apply_document_tx"));
        if !dispatch {
            return store.apply_document_tx(self, db, identity, document, expected_job, prepared);
        }
        if prepared.is_some() {
            return Err(Error::Value("overridden snapshot transaction is unsupported for schema 7".into()));
        }
        Python::attach(|py| {
            let kw = PyDict::new(py);
            kw.set_item("expected_job", match expected_job {
                Some(j) => pyv(py, j)?,
                None => py.None(),
            })
            .map_err(py_err)?;
            let dbo = db_object(py, db).map_err(py_err)?;
            let r = self.call(py, "_apply_document_tx", (dbo, identity, pyv(py, document)?), Some(&kw))?;
            r.is_truthy().map_err(py_err)
        })
    }

    #[cfg(feature = "python")]
    fn apply_favorite_tx(&self, _store: &StoreCore, db: &dyn Sql, identity: &str, document: &Val) -> R<()> {
        if !Python::attach(|py| self.overridden(py, "_apply_favorite_tx")) {
            return crate::storage::apply_favorite_tx(db, identity, document);
        }
        Python::attach(|py| {
            let dbo = db_object(py, db).map_err(py_err)?;
            self.call(py, "_apply_favorite_tx", (dbo, identity, pyv(py, document)?), None).map(|_| ())
        })
    }

    #[cfg(feature = "python")]
    fn guard_ink_job(&self, _store: &StoreCore, db: &dyn Sql, job: &Val) -> R<()> {
        if !Python::attach(|py| self.overridden(py, "guard_ink_job")) {
            return crate::ink_store::guard_ink_job_tx(db, job);
        }
        Python::attach(|py| {
            let kw = PyDict::new(py);
            kw.set_item("db", db_object(py, db).map_err(py_err)?).map_err(py_err)?;
            self.call(py, "guard_ink_job", (pyv(py, job)?,), Some(&kw)).map(|_| ())
        })
    }

    #[cfg(feature = "python")]
    fn ensure_no_active_ink(&self, db: &dyn Sql, identity: &str) -> R<()> {
        if !Python::attach(|py| self.overridden(py, "ensure_no_active_ink")) {
            return crate::ink_store::ensure_no_active_ink(db, identity);
        }
        Python::attach(|py| {
            let dbo = db_object(py, db).map_err(py_err)?;
            self.call(py, "ensure_no_active_ink", (dbo, identity), None).map(|_| ())
        })
    }

    #[cfg(feature = "python")]
    fn organize_cloud(&self, store: &StoreCore, identity: &str, changes: &Val) -> R<Val> {
        if !Python::attach(|py| self.overridden(py, "organize_cloud")) {
            return store.organize_cloud(self, identity, changes);
        }
        Python::attach(|py| {
            let r = self.call(py, "organize_cloud", (identity, pyv(py, changes)?), None)?;
            to_val(&r).map_err(py_err)
        })
    }

    #[cfg(feature = "python")]
    fn apply_document(&self, store: &StoreCore, server_id: &Val, document: &Val) -> R<String> {
        if !Python::attach(|py| self.overridden(py, "apply_document")) {
            return store.apply_document(self, server_id, document, None);
        }
        Python::attach(|py| {
            let r = self.call(py, "apply_document", (pyv(py, server_id)?, pyv(py, document)?), None)?;
            r.extract::<String>().map_err(py_err)
        })
    }

    fn normalize_model(&self, model: &Val) -> R<Val> {
        // rs-editor is STABLE: call its pure-Rust normalize_model directly.
        use _editor::rich::Error as E;
        match _editor::rich::normalize_model(&to_editor(model)) {
            Ok(v) => from_editor(&v),
            Err(e) => Err(Python::attach(|py| {
                let err = match e {
                    E::Document(m) => py
                        .import("samsung_notes.document")
                        .and_then(|d| d.getattr("DocumentError"))
                        .and_then(|t| t.call1((m,)))
                        .map(PyErr::from_value)
                        .unwrap_or_else(|x| x),
                    E::Payload(m) => py
                        .import("tools.notes_payload")
                        .and_then(|d| d.getattr("PayloadError"))
                        .and_then(|t| t.call1((m,)))
                        .map(PyErr::from_value)
                        .unwrap_or_else(|x| x),
                    E::Value(m) | E::Parse(m) => pyo3::exceptions::PyValueError::new_err(m),
                    E::Type(m) => pyo3::exceptions::PyTypeError::new_err(m),
                    E::Overflow(m) => pyo3::exceptions::PyOverflowError::new_err(m),
                    E::Attribute(m) => pyo3::exceptions::PyAttributeError::new_err(m),
                    E::External { payload, message, .. } => match payload.downcast::<PyErr>() {
                        Ok(err) => *err,
                        Err(_) => pyo3::exceptions::PyValueError::new_err(message),
                    },
                };
                py_err(err)
            })),
        }
    }

    #[cfg(feature = "python")]
    fn page_background_state(&self, document: Option<&Val>) -> R<Val> {
        // rs-ops core (STABLE) with its own PyDeps; same dict as page_style.page_background_state.
        Python::attach(|py| {
            let run = || -> PyResult<Py<PyAny>> {
                let deps = OpsDeps { py };
                let doc = match document {
                    Some(d) => Some(to_py(py, d)?),
                    None => None,
                };
                let arg: Option<&dyn std::any::Any> = doc.as_ref().map(|d| d as &dyn std::any::Any);
                let s = _ops::page_style::page_background_state(&deps, arg).py(py)?;
                let d = PyDict::new(py);
                d.set_item("backgroundArgb", s.background_argb)?;
                d.set_item("editable", s.editable)?;
                d.set_item("reason", &s.reason)?;
                if s.editable {
                    let rows = pyo3::types::PyList::empty(py);
                    for row in _ops::page_style::PAGE_PALETTE.iter() {
                        let r = pyo3::types::PyList::empty(py);
                        for (c, n) in row {
                            let item = PyDict::new(py);
                            item.set_item("argb", *c)?;
                            item.set_item("name", *n)?;
                            r.append(item)?;
                        }
                        rows.append(r)?;
                    }
                    d.set_item("palette", rows)?;
                }
                Ok(d.into_any().unbind())
            };
            let d = run().map_err(py_err)?;
            to_val(d.bind(py)).map_err(py_err)
        })
    }

    #[cfg(feature = "python")]
    fn prepare_page_background(&self, document: &Val, color_argb: i64) -> FaultResult<()> {
        faulting(|py| {
            let doc = to_py(py, document)?;
            _ops::page_style::prepare_page_background(&OpsDeps { py }, &doc, Some(color_argb as i128), Some(DRY_RUN_MS as i128)).py(py)?;
            Ok(())
        })
    }

    fn prepare_pdf_pages(&self, document: &Val, file_sha256: &Val, file_name: &Val, page_sizes: &Val, page_indices: &Val, template_type: &Val) -> FaultResult<()> {
        use _ops::ink_erase::{exact_int, opt_str};
        faulting(|py| {
            let sizes_obj = to_py(py, page_sizes)?;
            let mut sizes = Vec::new();
            for item in sizes_obj.bind(py).try_iter()? {
                let item = item?;
                let pair: Vec<Bound<PyAny>> = item.try_iter()?.collect::<PyResult<_>>()?;
                if pair.len() != 2 {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!("expected 2 values to unpack (got {})", pair.len())));
                }
                sizes.push((pair[0].extract::<f64>()?, pair[1].extract::<f64>()?));
            }
            let template = to_py(py, template_type)?;
            let indices = to_py(py, page_indices)?;
            let args = _ops::page_template::PdfArgs {
                file_sha256: opt_str(to_py(py, file_sha256)?.bind(py)),
                upload_token: Some(String::new()),
                file_name: opt_str(to_py(py, file_name)?.bind(py)),
                page_sizes: sizes,
                now_ms: Some(DRY_RUN_MS as i128),
                template_type: if template.is_none(py) { None } else { Some(exact_int(template.bind(py))) },
                page_indices: if indices.is_none(py) {
                    None
                } else {
                    Some(indices.bind(py).try_iter()?.map(|v| v.map(|v| exact_int(&v))).collect::<PyResult<_>>()?)
                },
                ids: None,
            };
            let doc = to_py(py, document)?;
            _ops::page_template::prepare_pdf_pages(&OpsDeps { py }, &doc, &args).py(py)?;
            Ok(())
        })
    }

    #[cfg(feature = "python")]
    fn page_ops_state(&self, document: Option<&Val>) -> R<Val> {
        // Same dict as page_ops.page_ops_state.
        Python::attach(|py| {
            let run = || -> PyResult<Py<PyAny>> {
                let doc = match document {
                    Some(d) => Some(to_py(py, d)?),
                    None => None,
                };
                let arg: Option<&dyn std::any::Any> = doc.as_ref().map(|d| d as &dyn std::any::Any);
                let s = _ops::page_ops::page_ops_state(&OpsDeps { py }, arg).py(py)?;
                let d = PyDict::new(py);
                d.set_item("editable", s.editable)?;
                d.set_item("reason", &s.reason)?;
                let pages = pyo3::types::PyList::empty(py);
                for r in &s.pages {
                    let p = PyDict::new(py);
                    p.set_item("id", &r.id)?;
                    p.set_item("index", r.index)?;
                    p.set_item("width", r.width)?;
                    p.set_item("height", r.height)?;
                    p.set_item("hasStrokes", r.has_strokes)?;
                    p.set_item("deletable", r.deletable)?;
                    p.set_item("reason", &r.reason)?;
                    pages.append(p)?;
                }
                d.set_item("pages", pages)?;
                if s.editable {
                    d.set_item("canReorder", s.can_reorder)?;
                    d.set_item("canAdd", s.can_add)?;
                }
                Ok(d.into_any().unbind())
            };
            let d = run().map_err(py_err)?;
            to_val(d.bind(py)).map_err(py_err)
        })
    }

    #[cfg(feature = "python")]
    fn prepare_add_page(&self, document: &Val, after_index: &Val) -> FaultResult<()> {
        faulting(|py| {
            let doc = to_py(py, document)?;
            let after = _ops::ink_erase::exact_int(to_py(py, after_index)?.bind(py));
            _ops::page_ops::prepare_add_page(&OpsDeps { py }, &doc, after, Some(DRY_RUN_MS as i128), None, None, None, None).py(py)?;
            Ok(())
        })
    }

    #[cfg(feature = "python")]
    fn prepare_add_shape(&self, document: &Val, spec: &Val) -> FaultResult<Val> {
        faulting_val(|py| {
            let doc = to_py(py, document)?;
            crate::shapes::dry_run(&OpsDeps { py }, &doc, spec).py(py)
        })
    }

    #[cfg(feature = "python")]
    fn prepare_delete_page(&self, document: &Val, page_ids: &Val) -> FaultResult<()> {
        let ids: Option<Vec<Option<String>>> = match page_ids {
            Val::Str(s) => Some(vec![Some(s.clone())]),
            Val::List(items) => Some(items.iter().map(|v| v.as_str().map(String::from)).collect()),
            Val::Dict(items) => Some(items.iter().map(|(k, _)| Some(k.clone())).collect()),
            _ => None,
        };
        faulting(|py| {
            let Some(ids) = ids else {
                // Non-iterable ids: the module defers that TypeError until after
                // its snapshot load; keep that exact order by calling it.
                return not_iterable_after_load(py, document, page_ids);
            };
            let doc = to_py(py, document)?;
            _ops::page_ops::prepare_delete_page(&OpsDeps { py }, &doc, ids, Some(DRY_RUN_MS as i128)).py(py)?;
            Ok(())
        })
    }

    #[cfg(feature = "python")]
    fn prepare_reorder_pages(&self, document: &Val, page_ids: &Val) -> FaultResult<()> {
        let ids: Option<Vec<Option<String>>> = match page_ids {
            Val::Str(s) => Some(s.chars().map(|c| Some(c.to_string())).collect()),
            Val::List(items) => Some(items.iter().map(|v| v.as_str().map(String::from)).collect()),
            Val::Dict(items) => Some(items.iter().map(|(k, _)| Some(k.clone())).collect()),
            _ => None,
        };
        faulting(|py| {
            let Some(ids) = ids else {
                return not_iterable_after_load(py, document, page_ids);
            };
            let doc = to_py(py, document)?;
            _ops::page_ops::prepare_reorder_pages(&OpsDeps { py }, &doc, ids, Some(DRY_RUN_MS as i128)).py(py)?;
            Ok(())
        })
    }

    fn prepare_ink_delete(&self, document: &Val, page_id: &Val, layer_id: &Val, stroke_ids: &Val) -> FaultResult<()> {
        use _ops::ink_erase::{lazy_ids, opt_str};
        faulting(|py| {
            let doc = to_py(py, document)?;
            let (p, l) = (opt_str(to_py(py, page_id)?.bind(py)), opt_str(to_py(py, layer_id)?.bind(py)));
            let ids = lazy_ids(to_py(py, stroke_ids)?.bind(py));
            let edit = _ops::ink_erase::prepare_ink_delete(&OpsDeps { py }, &doc, p.as_deref(), l.as_deref(), &ids.0, Some(DRY_RUN_MS as i128));
            ids.finish(py, edit)?;
            Ok(())
        })
    }

    fn prepare_ink_move(&self, document: &Val, page_id: &Val, layer_id: &Val, stroke_ids: &Val, dx: f64, dy: f64) -> FaultResult<()> {
        use _ops::ink_erase::{lazy_ids, opt_str};
        faulting(|py| {
            let doc = to_py(py, document)?;
            let (p, l) = (opt_str(to_py(py, page_id)?.bind(py)), opt_str(to_py(py, layer_id)?.bind(py)));
            let ids = lazy_ids(to_py(py, stroke_ids)?.bind(py));
            let edit = _ops::ink_erase::prepare_ink_move(&OpsDeps { py }, &doc, p.as_deref(), l.as_deref(), &ids.0, Some(dx), Some(dy), Some(DRY_RUN_MS as i128));
            ids.finish(py, edit)?;
            Ok(())
        })
    }

    #[cfg(feature = "python")]
    fn prepare_first_stroke(&self, snapshot: &Py<PyAny>, page_id: &str, points: &Val, color_argb: i64, size: f64, now_ms: i64, new_stroke_id: &str) -> FaultResult<()> {
        faulting(|py| {
            let samples = samples_from_points(py, to_py(py, points)?.bind(py))?;
            _ops::ink_first_stroke::prepare_first_stroke(
                &OpsDeps { py },
                snapshot,
                Some(page_id),
                Some(&samples as &dyn std::any::Any),
                Some(color_argb as i128),
                Some(size),
                Some(now_ms as i128),
                Some(Some(new_stroke_id)),
            )
            .py(py)?;
            Ok(())
        })
    }

    #[cfg(feature = "python")]
    fn prepare_highlighter_stroke(&self, snapshot: &Py<PyAny>, page_id: &str, points: &Val, color_argb: i64, size: f64, now_ms: i64, new_stroke_id: &str) -> FaultResult<()> {
        faulting(|py| {
            let samples = samples_from_points(py, to_py(py, points)?.bind(py))?;
            _ops::ink_highlighter::prepare_highlighter_stroke(
                &OpsDeps { py },
                snapshot,
                Some(page_id),
                Some(&samples as &dyn std::any::Any),
                Some(color_argb as i128),
                Some(size),
                Some(now_ms as i128),
                Some(Some(new_stroke_id)),
            )
            .py(py)?;
            Ok(())
        })
    }

    #[cfg(feature = "python")]
    fn snapshot_from_dict(&self, document: &Val) -> R<Py<PyAny>> {
        Python::attach(|py| {
            // Boundary class lookup (the class is rs-doc's), like rs-ops' bridge.
            let cls = py.import("samsung_notes.document").and_then(|m| m.getattr("DocumentSnapshot")).map_err(py_err)?;
            Ok(cls.call_method1("from_dict", (pyv(py, document)?,)).map_err(py_err)?.unbind())
        })
    }

    #[cfg(feature = "python")]
    fn prepare_ink_append(&self, snapshot: &Py<PyAny>, recipe: i64, stroke: &Val, now_ms: i64) -> FaultResult<Val> {
        // rs-ink pure append API (STABLE); the snapshot's four fields are read
        // from the Python DocumentSnapshot (rs-doc's class) at the boundary.
        faulting_val(|py| {
            let snap = snapshot.bind(py);
            let resource_xml: Vec<u8> = snap.getattr("resource_xml")?.extract()?;
            let raw_body: Vec<u8> = snap.getattr("raw_body")?.extract()?;
            let content_type: String = snap.getattr("content_type")?.extract()?;
            let commit_id: Option<String> = snap.getattr("commit_id")?.extract()?;
            let append = rs_ink::core::prepare::AppendSnapshot {
                resource_xml: &resource_xml,
                raw_body: &raw_body,
                content_type: &content_type,
                commit_id: commit_id.as_deref(),
            };
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
                        _ => return Err(pyo3::exceptions::PyOverflowError::new_err("Python int too large to convert to C long")),
                    });
                }
            }
            let samples = rs_ink::core::binary::RawSamples::from_parts(&xy, &pressures, &times);
            let limits = rs_doc::pyapi::payload_limits(py)?;
            let source = stroke.get_or_null("sourceStrokeId");
            let new_id = stroke.get_or_null("id");
            let (source, new_id) = (source.as_str().unwrap_or(""), new_id.as_str().unwrap_or(""));
            let edit = if recipe == 3 {
                let color = stroke.get_or_null("colorARGB").as_i64().unwrap_or(0) as u32;
                let size = stroke.get_or_null("size").as_f64().unwrap_or(f64::NAN);
                rs_ink::core::prepare::append_v3(&append, source, new_id, &samples, now_ms, color, size, Some(&limits))
            } else {
                rs_ink::core::prepare::append_v2(&append, source, new_id, &samples, now_ms, Some(&limits))
            }
            .map_err(|e| rs_ink::pyapi::ink_error(py, e))?;
            Ok(Val::Str(edit.page_id.clone()))
        })
    }
}

// ---------------------------------------------------------------- module functions

#[cfg(feature = "python")]
#[pyfunction]
pub fn now_ms() -> i64 {
    db::now_ms()
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn private_directory(py: Python<'_>, path: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let p = path_obj(py, path)?;
    let pb: PathBuf = p.bind(py).extract()?;
    ok(py, crate::storage::private_directory(&pb))?;
    Ok(p)
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn normalize_commit(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
    let v = v(value)?;
    ok(py, crate::storage::normalize_commit_val(&v).map_err(|e| match e {
        Error::Type(m) => Error::Attribute(m),
        other => other,
    }))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn favorite_from_document(py: Python<'_>, document: &Bound<'_, PyAny>) -> PyResult<Option<bool>> {
    if !document.is_instance_of::<PyDict>() {
        // Only mappings with .get(): read the one attribute the original needs.
        let encoded = document.call_method1("get", ("resource_xml",))?;
        let d = Val::dict(vec![("resource_xml", to_val(&encoded).unwrap_or(Val::Int(0)))]);
        return ok(py, crate::organization_store::favorite_from_document(&d));
    }
    let resource = document.cast::<PyDict>()?.get_item("resource_xml")?;
    let d = Val::dict(vec![(
        "resource_xml",
        match resource {
            Some(r) => to_val(&r).unwrap_or(Val::Int(0)),
            None => Val::Null,
        },
    )]);
    ok(py, crate::organization_store::favorite_from_document(&d))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn migrate(py: Python<'_>, db: &Bound<'_, PyAny>) -> PyResult<()> {
    let db = DbArg::from_py(db)?;
    ok(py, crate::organization_store::migrate(db.sql()))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn samples_from_points(py: Python<'_>, points: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let mut xy = Vec::new();
    let mut pressures = Vec::new();
    let mut times = Vec::new();
    for p in points.try_iter()? {
        let p = p?;
        xy.push(PyTuple::new(py, [p.get_item("x")?, p.get_item("y")?])?.into_any());
    }
    for p in points.try_iter()? {
        pressures.push(p?.get_item("pressure")?);
    }
    for p in points.try_iter()? {
        times.push(p?.get_item("timeMs")?);
    }
    // Boundary class lookup (rs-ink's dataclass), not logic.
    let cls = py.import("samsung_notes.ink_editor")?.getattr("StrokeSamples")?;
    Ok(cls
        .call1((PyTuple::new(py, xy)?, PyTuple::new(py, pressures)?, PyTuple::new(py, times)?))?
        .unbind())
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn validate_batch(
    py: Python<'_>,
    strokes: &Bound<'_, PyAny>,
    projection: &Bound<'_, PyAny>,
    snapshot: &Bound<'_, PyAny>,
    stamp: i64,
) -> PyResult<Py<PyAny>> {
    // A host without a store object: only collaborators are used here.
    let host = PyHost { obj: py.None() };
    let (s, p) = (v(strokes)?, v(projection)?);
    let snap = snapshot.clone().unbind();
    out(py, crate::ink_store::validate_batch(&host, &s, &p, &snap, stamp))
}

#[cfg(feature = "python")]
/// Empty marker class kept for import compatibility: the mixin's methods
/// live on the single Rust `Store` class.
pub fn add_marker(m: &Bound<'_, PyModule>, name: &str, module: &str) -> PyResult<()> {
    let py = m.py();
    let ns = PyDict::new(py);
    ns.set_item("__module__", module)?;
    ns.set_item("__doc__", format!("{} methods are implemented by the Rust Store class (samsung_notes.storage.Store).", name))?;
    let cls = py.get_type::<PyType>().call1((name, PyTuple::new(py, [py.get_type::<pyo3::types::PyAny>()])?, ns))?;
    m.add(name, cls)
}

// ---------------------------------------------------------------- Store

#[cfg(feature = "python")]
/// Python mapping row (sqlite3.Row, dict, our Row) for Store.public.
struct PyRowGet<'py>(Bound<'py, PyAny>);

#[cfg(feature = "python")]
impl RowGet for PyRowGet<'_> {
    #[cfg(feature = "python")]
    fn get(&self, key: &str) -> R<Val> {
        let item = self.0.get_item(key).map_err(py_err)?;
        to_val(&item).map_err(py_err)
    }
}

#[cfg(feature = "python")]
#[pyclass(subclass, dict, module = "samsung_notes.storage", name = "Store")]
pub struct Store {
    core: StoreCore,
    #[pyo3(get, set)]
    directory: Py<PyAny>,
    #[pyo3(get, set)]
    path: Py<PyAny>,
}

#[cfg(feature = "python")]
fn host(slf: &Bound<'_, Store>) -> PyHost {
    PyHost::new(slf.as_any())
}

#[cfg(feature = "python")]
fn tuple3(py: Python<'_>, r: (Val, i64, Val)) -> PyResult<Py<PyAny>> {
    Ok(PyTuple::new(py, [to_py(py, &r.0)?, r.1.into_pyobject(py)?.into_any().unbind(), to_py(py, &r.2)?])?.into_any().unbind())
}

#[cfg(feature = "python")]
#[pymethods]
impl Store {
    #[cfg(feature = "python")]
    #[classattr]
    const PUBLIC_COLUMNS: &'static str = crate::storage::PUBLIC_COLUMNS;

    #[cfg(feature = "python")]
    #[new]
    fn new(py: Python<'_>, directory: &Bound<'_, PyAny>) -> PyResult<Self> {
        let dir = path_obj(py, directory)?;
        let pb: PathBuf = dir.bind(py).extract()?;
        let core = ok(py, StoreCore::open(&pb))?;
        let path = dir.bind(py).call_method1("__truediv__", ("library.sqlite3",))?.unbind();
        Ok(Store { core, directory: dir, path })
    }

    #[cfg(feature = "python")]
    fn connect(&self, py: Python<'_>) -> PyResult<Connection> {
        ok(py, Connection::open(&self.core.path))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (key, default = None))]
    fn meta(&self, py: Python<'_>, key: &str, default: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        match ok(py, self.core.meta(key))? {
            Some(v) => to_py(py, &v),
            None => Ok(default.unwrap_or_else(|| py.None())),
        }
    }

    #[cfg(feature = "python")]
    fn set_meta(&self, py: Python<'_>, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let value = v(value)?;
        ok(py, self.core.set_meta(key, &value))
    }

    #[cfg(feature = "python")]
    fn raw(&self, py: Python<'_>, note_id: &str) -> PyResult<Py<PyAny>> {
        out(py, self.core.raw(note_id))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    #[pyo3(signature = (row, detail = false))]
    fn public(py: Python<'_>, row: &Bound<'_, PyAny>, detail: bool) -> PyResult<Py<PyAny>> {
        out(py, crate::storage::public(&PyRowGet(row.clone()), detail))
    }

    #[cfg(feature = "python")]
    fn get(&self, py: Python<'_>, note_id: &str) -> PyResult<Py<PyAny>> {
        out(py, self.core.get(note_id))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (query = "", filter = None, folder = None))]
    fn list(&self, py: Python<'_>, query: &str, filter: Option<&Bound<'_, PyAny>>, folder: Option<&Bound<'_, PyAny>>) -> PyResult<Py<PyAny>> {
        let filter = match filter {
            None => "all".to_string(),
            Some(f) => match f.cast::<PyString>() {
                Ok(s) => s.to_str()?.to_string(),
                Err(_) => "\u{0}".to_string(),
            },
        };
        let folder = match folder {
            Some(f) if !f.is_none() => Some(v(f)?),
            _ => None,
        };
        let items = ok(py, self.core.list(query, &filter, folder.as_ref()))?;
        out(py, Ok(Val::List(items)))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    fn validate_text(py: Python<'_>, title: &Bound<'_, PyAny>, body: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, crate::storage::validate_text(&v(title)?, &v(body)?))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (title = None, body = None))]
    fn create(&self, py: Python<'_>, title: Option<&Bound<'_, PyAny>>, body: Option<&Bound<'_, PyAny>>) -> PyResult<Py<PyAny>> {
        let t = title.map(v).transpose()?.unwrap_or(Val::s(""));
        let b = body.map(v).transpose()?.unwrap_or(Val::s(""));
        out(py, self.core.create(&t, &b))
    }

    #[cfg(feature = "python")]
    fn save(slf: &Bound<'_, Self>, identity: &str, title: &Bound<'_, PyAny>, body: &Bound<'_, PyAny>, expected_revision: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let (t, b, r) = (v(title)?, v(body)?, v(expected_revision)?);
        out(py, slf.borrow().core.save(&host(slf), identity, &t, &b, &r))
    }

    #[cfg(feature = "python")]
    fn organize(slf: &Bound<'_, Self>, identity: &str, changes: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let c = v(changes)?;
        let h = host(slf);
        out(py, h.organize_cloud(&slf.borrow().core, identity, &c))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (identity, supported, *, raw_sha256))]
    fn title_capability(&self, py: Python<'_>, identity: &str, supported: &Bound<'_, PyAny>, raw_sha256: &Bound<'_, PyAny>) -> PyResult<()> {
        let s = supported.is_truthy()?;
        ok(py, self.core.title_capability(identity, s, &v(raw_sha256)?))
    }

    #[cfg(feature = "python")]
    fn raw_state(&self, py: Python<'_>, identity: &str) -> PyResult<Py<PyAny>> {
        out(py, self.core.raw_state(identity))
    }

    #[cfg(feature = "python")]
    fn save_title(slf: &Bound<'_, Self>, identity: &str, title: &Bound<'_, PyAny>, expected_revision: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let (t, r) = (v(title)?, v(expected_revision)?);
        out(py, slf.borrow().core.save_title(&host(slf), identity, &t, &r))
    }

    #[cfg(feature = "python")]
    fn page_style(slf: &Bound<'_, Self>, identity: &str) -> PyResult<Py<PyAny>> {
        out(slf.py(), slf.borrow().core.page_style(&host(slf), identity))
    }

    #[cfg(feature = "python")]
    fn save_page_background(slf: &Bound<'_, Self>, identity: &str, color_argb: &Bound<'_, PyAny>, expected_revision: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let (c, r) = (v(color_argb)?, v(expected_revision)?);
        out(slf.py(), slf.borrow().core.save_page_background(&host(slf), identity, &c, &r))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (identity, expected_revision, *, file_sha256, file_name, page_sizes, page_indices = None, template_type = None))]
    #[allow(clippy::too_many_arguments)]
    fn save_pdf_pages(
        slf: &Bound<'_, Self>,
        identity: &str,
        expected_revision: &Bound<'_, PyAny>,
        file_sha256: &Bound<'_, PyAny>,
        file_name: &Bound<'_, PyAny>,
        page_sizes: &Bound<'_, PyAny>,
        page_indices: Option<&Bound<'_, PyAny>>,
        template_type: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let indices = page_indices.map(v).transpose()?.unwrap_or(Val::Null);
        let template = template_type.map(v).transpose()?.unwrap_or(Val::Null);
        let (r, sha, name, sizes) = (v(expected_revision)?, v(file_sha256)?, v(file_name)?, v(page_sizes)?);
        out(slf.py(), slf.borrow().core.save_pdf_pages(&host(slf), identity, &r, &sha, &name, &sizes, &indices, &template))
    }

    #[cfg(feature = "python")]
    fn pdf_page_job(&self, py: Python<'_>, job: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        out(py, self.core.pdf_page_job(&v(job)?))
    }

    #[cfg(feature = "python")]
    fn page_ops(slf: &Bound<'_, Self>, identity: &str) -> PyResult<Py<PyAny>> {
        out(slf.py(), slf.borrow().core.page_ops(&host(slf), identity))
    }

    #[cfg(feature = "python")]
    fn save_page_op(slf: &Bound<'_, Self>, identity: &str, op: &Bound<'_, PyAny>, spec: &Bound<'_, PyAny>, expected_revision: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let (o, s, r) = (v(op)?, v(spec)?, v(expected_revision)?);
        out(slf.py(), slf.borrow().core.save_page_op(&host(slf), identity, &o, &s, &r))
    }

    #[cfg(feature = "python")]
    fn page_op_job(&self, py: Python<'_>, job: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let (op, spec) = ok(py, self.core.page_op_job(&v(job)?))?;
        Ok(PyTuple::new(py, [to_py(py, &op)?, to_py(py, &spec)?])?.into_any().unbind())
    }

    #[cfg(feature = "python")]
    fn page_style_job_color(&self, py: Python<'_>, job: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        out(py, self.core.page_style_job_color(&v(job)?))
    }

    #[cfg(feature = "python")]
    fn counts(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        out(py, self.core.counts())
    }

    #[cfg(feature = "python")]
    fn by_server(&self, py: Python<'_>, server_id: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let r = ok(py, self.core.by_server(&v(server_id)?))?;
        out(py, Ok(r.unwrap_or(Val::Null)))
    }

    #[cfg(feature = "python")]
    fn stage_change(&self, py: Python<'_>, change: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
        ok(py, self.core.stage_change(&v(change)?))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, document, *, expected_job = None))]
    fn apply_document(slf: &Bound<'_, Self>, server_id: &Bound<'_, PyAny>, document: &Bound<'_, PyAny>, expected_job: Option<&Bound<'_, PyAny>>) -> PyResult<String> {
        let job = match expected_job {
            Some(j) if !j.is_none() => Some(v(j)?),
            _ => None,
        };
        let (s, d) = (v(server_id)?, v(document)?);
        ok(slf.py(), slf.borrow().core.apply_document(&host(slf), &s, &d, job.as_ref()))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    fn _apply_favorite_tx(py: Python<'_>, db: &Bound<'_, PyAny>, identity: &str, document: &Bound<'_, PyAny>) -> PyResult<()> {
        let db = DbArg::from_py(db)?;
        ok(py, crate::storage::apply_favorite_tx(db.sql(), identity, &v(document)?))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (db, identity, document, *, expected_job = None))]
    fn _apply_document_tx(slf: &Bound<'_, Self>, db: &Bound<'_, PyAny>, identity: &str, document: &Bound<'_, PyAny>, expected_job: Option<&Bound<'_, PyAny>>) -> PyResult<bool> {
        let db = DbArg::from_py(db)?;
        let job = match expected_job {
            Some(j) if !j.is_none() => Some(v(j)?),
            _ => None,
        };
        let d = v(document)?;
        ok(slf.py(), slf.borrow().core.apply_document_tx(&host(slf), db.sql(), identity, &d, job.as_ref(), None))
    }

    #[cfg(feature = "python")]
    fn content_error(&self, py: Python<'_>, identity: &str, message: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.content_error(identity, &v(message)?))
    }

    #[cfg(feature = "python")]
    fn next_job(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let r = ok(py, self.core.next_job())?;
        out(py, Ok(r.unwrap_or(Val::Null)))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (job, state, message = None))]
    fn job_state(&self, py: Python<'_>, job: &Bound<'_, PyAny>, state: &Bound<'_, PyAny>, message: Option<&Bound<'_, PyAny>>) -> PyResult<bool> {
        let m = message.map(v).transpose()?.unwrap_or(Val::Null);
        ok(py, self.core.job_state(&v(job)?, &v(state)?, &m))
    }

    #[cfg(feature = "python")]
    fn complete_job(slf: &Bound<'_, Self>, job: &Bound<'_, PyAny>, result: &Bound<'_, PyAny>) -> PyResult<()> {
        let (j, r) = (v(job)?, v(result)?);
        ok(slf.py(), slf.borrow().core.complete_job(&host(slf), &j, &r))
    }

    #[cfg(feature = "python")]
    fn resolve(slf: &Bound<'_, Self>, identity: &str, strategy: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let s = v(strategy)?;
        out(slf.py(), slf.borrow().core.resolve(&host(slf), identity, &s))
    }

    // ---- OrganizationStore

    #[cfg(feature = "python")]
    fn organization_node(&self, py: Python<'_>, resource_id: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let r = ok(py, self.core.organization_node(&v(resource_id)?))?;
        out(py, Ok(r.unwrap_or(Val::Null)))
    }

    #[cfg(feature = "python")]
    fn folders(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        out(py, self.core.folders())
    }

    #[cfg(feature = "python")]
    fn apply_folder_graph(&self, py: Python<'_>, inventory: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.apply_folder_graph(&v(inventory)?))
    }

    #[cfg(feature = "python")]
    fn organize_cloud(slf: &Bound<'_, Self>, identity: &str, changes: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let c = v(changes)?;
        out(slf.py(), slf.borrow().core.organize_cloud(&host(slf), identity, &c))
    }

    #[cfg(feature = "python")]
    fn register_note_graph(&self, py: Python<'_>, identity: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.register_note_graph(&v(identity)?))
    }

    #[cfg(feature = "python")]
    fn next_organization_job(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let r = ok(py, self.core.next_organization_job())?;
        out(py, Ok(r.unwrap_or(Val::Null)))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (job, state, message = None))]
    fn organization_state(&self, py: Python<'_>, job: &Bound<'_, PyAny>, state: &Bound<'_, PyAny>, message: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let m = message.map(v).transpose()?.unwrap_or(Val::Null);
        ok(py, self.core.organization_state(&v(job)?, &v(state)?, &m))
    }

    #[cfg(feature = "python")]
    fn organization_completed(&self, py: Python<'_>, job: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.organization_completed(&v(job)?))
    }

    #[cfg(feature = "python")]
    fn apply_organization_node(&self, py: Python<'_>, node: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.apply_organization_node(&v(node)?))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    fn _write_node(py: Python<'_>, db: &Bound<'_, PyAny>, node: &Bound<'_, PyAny>) -> PyResult<()> {
        let db = DbArg::from_py(db)?;
        ok(py, crate::organization_store::write_node(db.sql(), &v(node)?))
    }

    #[cfg(feature = "python")]
    fn folder_completed(&self, py: Python<'_>, job: &Bound<'_, PyAny>, node: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.folder_completed(&v(job)?, &v(node)?))
    }

    #[cfg(feature = "python")]
    fn apply_favorite(&self, py: Python<'_>, identity: &str, document: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.apply_favorite(identity, &v(document)?))
    }

    #[cfg(feature = "python")]
    fn folder(&self, py: Python<'_>, identity: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        out(py, self.core.folder(&v(identity)?))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    fn _folder_name(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
        ok(py, crate::organization_store::folder_name(&v(value)?))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (name, parent = None))]
    fn create_folder(&self, py: Python<'_>, name: &Bound<'_, PyAny>, parent: Option<&Bound<'_, PyAny>>) -> PyResult<Py<PyAny>> {
        let p = parent.map(v).transpose()?.unwrap_or(Val::Null);
        out(py, self.core.create_folder(&v(name)?, &p))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (parent, *, identity = None))]
    fn _check_folder_parent(&self, py: Python<'_>, parent: &Bound<'_, PyAny>, identity: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let i = identity.map(v).transpose()?;
        ok(py, self.core.check_folder_parent(&v(parent)?, i.as_ref()))
    }

    #[cfg(feature = "python")]
    fn update_folder(&self, py: Python<'_>, identity: &Bound<'_, PyAny>, changes: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        out(py, self.core.update_folder(&v(identity)?, &v(changes)?))
    }

    #[cfg(feature = "python")]
    fn next_folder_job(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let r = ok(py, self.core.next_folder_job())?;
        out(py, Ok(r.unwrap_or(Val::Null)))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (job, state, message = None))]
    fn folder_job_state(&self, py: Python<'_>, job: &Bound<'_, PyAny>, state: &Bound<'_, PyAny>, message: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let m = message.map(v).transpose()?.unwrap_or(Val::Null);
        ok(py, self.core.folder_job_state(&v(job)?, &v(state)?, &m))
    }

    #[cfg(feature = "python")]
    fn accept_remote_organization(&self, py: Python<'_>, identity: &str) -> PyResult<Py<PyAny>> {
        out(py, self.core.accept_remote_organization(identity))
    }

    #[cfg(feature = "python")]
    fn accept_remote_folder(&self, py: Python<'_>, identity: &Bound<'_, PyAny>, inventory: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let r = ok(py, self.core.accept_remote_folder(&v(identity)?, &v(inventory)?))?;
        out(py, Ok(r.unwrap_or(Val::Null)))
    }

    // ---- EditorStore

    #[cfg(feature = "python")]
    #[pyo3(signature = (identity, revision = None))]
    fn cached_editor(&self, py: Python<'_>, identity: &str, revision: Option<&Bound<'_, PyAny>>) -> PyResult<Py<PyAny>> {
        let rev = match revision {
            Some(r) if !r.is_none() => Some(v(r)?),
            _ => None,
        };
        let r = ok(py, self.core.cached_editor(identity, rev.as_ref()))?;
        out(py, Ok(r.unwrap_or(Val::Null)))
    }

    #[cfg(feature = "python")]
    fn cache_editor(&self, py: Python<'_>, identity: &str, revision: &Bound<'_, PyAny>, projection: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.cache_editor(identity, &v(revision)?, &v(projection)?))
    }

    #[cfg(feature = "python")]
    fn create_editor(slf: &Bound<'_, Self>, title: &Bound<'_, PyAny>, model: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let (t, m) = (v(title)?, v(model)?);
        out(slf.py(), slf.borrow().core.create_editor(&host(slf), &t, &m))
    }

    #[cfg(feature = "python")]
    fn save_editor(slf: &Bound<'_, Self>, identity: &str, title: &Bound<'_, PyAny>, model: &Bound<'_, PyAny>, expected_revision: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let (t, m, r) = (v(title)?, v(model)?, v(expected_revision)?);
        out(slf.py(), slf.borrow().core.save_editor(&host(slf), identity, &t, &m, &r))
    }

    #[cfg(feature = "python")]
    fn editor_job_model(&self, py: Python<'_>, job: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        out(py, self.core.editor_job_model(&v(job)?))
    }

    // ---- InkStore

    #[cfg(feature = "python")]
    #[staticmethod]
    fn ensure_no_active_ink(py: Python<'_>, db: &Bound<'_, PyAny>, identity: &str) -> PyResult<()> {
        let db = DbArg::from_py(db)?;
        ok(py, crate::ink_store::ensure_no_active_ink(db.sql(), identity))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (job, *, db = None))]
    fn guard_ink_job(&self, py: Python<'_>, job: &Bound<'_, PyAny>, db: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let job = v(job)?;
        match db {
            Some(d) if !d.is_none() => {
                let db = DbArg::from_py(d)?;
                ok(py, crate::ink_store::guard_ink_job_tx(db.sql(), &job))
            }
            _ => ok(py, self.core.with_db(|db| crate::ink_store::guard_ink_job_tx(db, &job))),
        }
    }

    #[cfg(feature = "python")]
    fn suspend_ink_job(&self, py: Python<'_>, job: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.suspend_ink_job(&v(job)?))
    }

    #[cfg(feature = "python")]
    fn cached_ink(&self, py: Python<'_>, identity: &str, revision: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let r = ok(py, self.core.cached_ink(identity, &v(revision)?))?;
        out(py, Ok(r.unwrap_or(Val::Null)))
    }

    #[cfg(feature = "python")]
    fn cache_ink(&self, py: Python<'_>, identity: &str, revision: &Bound<'_, PyAny>, projection: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.cache_ink(identity, &v(revision)?, &v(projection)?))
    }

    #[cfg(feature = "python")]
    fn save_ink_erase(slf: &Bound<'_, Self>, identity: &str, erase: &Bound<'_, PyAny>, expected_revision: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let (e, r) = (v(erase)?, v(expected_revision)?);
        out(slf.py(), slf.borrow().core.save_ink_erase(&host(slf), identity, &e, &r))
    }

    #[cfg(feature = "python")]
    fn ink_erase_job(&self, py: Python<'_>, job: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let r = ok(py, self.core.ink_erase_job(&v(job)?))?;
        tuple3(py, r)
    }

    #[cfg(feature = "python")]
    fn checkpoint_ink_erase(&self, py: Python<'_>, job: &Bound<'_, PyAny>, index: &Bound<'_, PyAny>, snapshot: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.checkpoint_ink_erase(&v(job)?, &v(index)?, &v(snapshot)?))
    }

    #[cfg(feature = "python")]
    fn save_ink_move(slf: &Bound<'_, Self>, identity: &str, r#move: &Bound<'_, PyAny>, expected_revision: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let (m, r) = (v(r#move)?, v(expected_revision)?);
        out(slf.py(), slf.borrow().core.save_ink_move(&host(slf), identity, &m, &r))
    }

    #[cfg(feature = "python")]
    fn ink_move_job(&self, py: Python<'_>, job: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let r = ok(py, self.core.ink_move_job(&v(job)?))?;
        tuple3(py, r)
    }

    #[cfg(feature = "python")]
    fn checkpoint_ink_move(&self, py: Python<'_>, job: &Bound<'_, PyAny>, index: &Bound<'_, PyAny>, snapshot: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.checkpoint_ink_move(&v(job)?, &v(index)?, &v(snapshot)?))
    }

    #[cfg(feature = "python")]
    fn ink_draft(&self, py: Python<'_>, identity: &str) -> PyResult<Py<PyAny>> {
        out(py, self.core.ink_draft(identity))
    }

    #[cfg(feature = "python")]
    fn save_ink_draft(&self, py: Python<'_>, identity: &str, revision: &Bound<'_, PyAny>, strokes: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        out(py, self.core.save_ink_draft(identity, &v(revision)?, &v(strokes)?))
    }

    #[cfg(feature = "python")]
    fn append_ink_draft(&self, py: Python<'_>, identity: &str, revision: &Bound<'_, PyAny>, strokes: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        out(py, self.core.append_ink_draft(identity, &v(revision)?, &v(strokes)?))
    }

    #[cfg(feature = "python")]
    fn save_ink_from_draft(slf: &Bound<'_, Self>, identity: &str, strokes: &Bound<'_, PyAny>, expected_revision: &Bound<'_, PyAny>, draft_ids: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        out(slf.py(), slf.borrow().core.save_ink_from_draft(&host(slf), identity, &v(strokes)?, &v(expected_revision)?, &v(draft_ids)?))
    }

    #[cfg(feature = "python")]
    fn save_ink(slf: &Bound<'_, Self>, identity: &str, strokes: &Bound<'_, PyAny>, expected_revision: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let (s, r) = (v(strokes)?, v(expected_revision)?);
        out(slf.py(), slf.borrow().core.save_ink(&host(slf), identity, &s, &r))
    }

    #[cfg(feature = "python")]
    fn ink_job(&self, py: Python<'_>, job: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let r = ok(py, self.core.ink_job(&v(job)?))?;
        tuple3(py, r)
    }

    #[cfg(feature = "python")]
    fn checkpoint_ink(&self, py: Python<'_>, job: &Bound<'_, PyAny>, index: &Bound<'_, PyAny>, snapshot: &Bound<'_, PyAny>) -> PyResult<()> {
        ok(py, self.core.checkpoint_ink(&v(job)?, &v(index)?, &v(snapshot)?))
    }
}
