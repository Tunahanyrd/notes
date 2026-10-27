//! Replacement for the Python module `thumbnails`.
use crate::pixels as px;
#[cfg(feature = "python")]
use crate::pdf_background::rgba_or_raise;
#[cfg(feature = "python")]
use crate::page_image::downsample;
#[cfg(feature = "python")]
use crate::preview_worker::encode;
#[cfg(feature = "python")]
use crate::pyhelp::{self, with_cm};
use crate::xarea;
#[cfg(feature = "python")]
use pyo3::exceptions::PyZeroDivisionError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::pybacked::PyBackedBytes;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyCFunction, PyDict, PyTuple};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

pub const WIDTH: i64 = 320;
pub const HEIGHT: i64 = 384;
pub const MAX_THUMB_BYTES: usize = (WIDTH * HEIGHT * 5 + 4096) as usize;

/// thumbnails.thumbnail_size: Python min/round (ties to even) on floats.
pub fn size(width: i64, height: i64) -> Option<(i64, i64)> {
    if width == 0 || height == 0 {
        return None;
    }
    let ratio = 1f64.min(WIDTH as f64 / width as f64).min(HEIGHT as f64 / height as f64);
    let scale = |v: i64| if ratio == 1.0 { v } else { (v as f64 * ratio).round_ties_even() as i64 };
    Some((scale(width).max(1), scale(height).max(1)))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn thumbnail_size(width: i64, height: i64) -> PyResult<(i64, i64)> {
    size(width, height).ok_or_else(|| PyZeroDivisionError::new_err("division by zero"))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature=(png, width, height, *, dark=false))]
pub fn downsample_page<'py>(py: Python<'py>, png: PyBackedBytes, width: i64, height: i64, dark: bool) -> PyResult<Bound<'py, PyBytes>> {
    let source = rgba_or_raise(py, &png, width, height)?;
    let (tw, th) = thumbnail_size(width, height)?;
    let out = downsample(py, &source, width as usize, height as usize, tw as usize, th as usize, if dark { 0 } else { 255 })?;
    encode(py, tw, th, &out)
}

#[cfg(feature = "python")]
#[pyclass(subclass, dict, module="samsung_notes.thumbnails", name="ThumbnailManager")]
pub struct ThumbnailManager {}

#[cfg(feature = "python")]
fn conflict(py: Python<'_>) -> PyErr {
    xarea::store_error(py, "revision_conflict", "Not değişti; güncel önizleme alınmalı.", 409)
}

#[cfg(feature = "python")]
#[pymethods]
impl ThumbnailManager {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        ThumbnailManager {}
    }

    #[cfg(feature = "python")]
    fn __init__(slf: &Bound<'_, Self>, engine: Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let directory = engine.getattr("store")?.getattr("directory")?.div("thumbnails")?;
        slf.setattr("directory", xarea::private_directory(py, &directory)?)?;
        slf.setattr("slots", py.import("threading")?.getattr("BoundedSemaphore")?.call1((2,))?)?;
        slf.setattr("engine", engine)?;
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _revision(slf: &Bound<'_, Self>, identity: &Bound<'_, PyAny>, revision: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let store = slf.getattr("engine")?.getattr("store")?;
        let row = with_cm(&store.call_method0("connect")?, |db| {
            db.call_method1("execute", ("SELECT revision,deleted_remote FROM notes WHERE id=?", (identity,)))?.call_method0("fetchone")
        })?;
        if row.is_none() || row.get_item("deleted_remote")?.is_truthy()? {
            return Err(xarea::store_error(py, "not_found", "Not önizlemesi bulunamadı.", 404));
        }
        if row.get_item("revision")?.ne(revision)? {
            return Err(conflict(py));
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn request<'py>(slf: &Bound<'py, Self>, identity: &Bound<'py, PyAny>, revision: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
        let py = slf.py();
        slf.call_method1("_revision", (identity, revision))?;
        let previews = slf.getattr("engine")?.getattr("previews")?;
        let source = previews.call_method1("_source", (identity,))?;
        let entry = previews.call_method1("_entry", (identity,))?;
        let candidate = entry.getattr("candidates")?.call_method1("get", (0,))?;
        if candidate.is_none() {
            return Err(xarea::store_error(py, "thumbnail_unavailable", "Bu notun ilk sayfa görüntüsü bulunmuyor.", 404));
        }
        let mut page = None;
        for p in entry.getattr("pages")?.try_iter()? {
            let p = p?;
            if p.get_item("index")?.eq(0)? {
                page = Some(p);
                break;
            }
        }
        let page = page.ok_or_else(|| pyo3::exceptions::PyStopIteration::new_err(()))?;
        let complete = page.call_method1("get", ("complete", true))?;
        let (cw, ch): (i64, i64) = (candidate.getattr("width")?.extract()?, candidate.getattr("height")?.extract()?);
        let (width, height) = thumbnail_size(cw, ch)?;
        let dark = page.get_item("darkMode")?.is_truthy()?;
        let stem: String = previews.call_method1("_path", (&candidate,))?.getattr("stem")?.extract()?;
        let recipe = format!("{stem}:thumbnail-v1:{}", if dark { "True" } else { "False" });
        let path = slf.getattr("directory")?.div(px::sha256_hex(recipe.as_bytes()) + ".png")?;
        let target: PathBuf = py.import("os")?.getattr("fspath")?.call1((&path,))?.extract::<String>()?.into();
        let this = slf.clone().unbind();
        let (identity_o, revision_o, source_o) = (identity.clone().unbind(), revision.clone().unbind(), source.clone().unbind());
        let guard = PyCFunction::new_closure(py, None, None, move |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
            let py = args.py();
            let this = this.bind(py);
            this.call_method1("_revision", (identity_o.bind(py), revision_o.bind(py)))?;
            if this.getattr("engine")?.getattr("previews")?.call_method1("_source", (identity_o.bind(py),))?.ne(source_o.bind(py))? {
                return Err(conflict(py));
            }
            Ok(())
        })?.into_any();
        let cached = py.detach(|| {
            let meta = std::fs::symlink_metadata(&target).ok()?;
            if meta.file_type().is_symlink() || !meta.is_file() || meta.len() as usize > MAX_THUMB_BYTES {
                return None;
            }
            let raw = std::fs::read(&target).ok()?;
            px::png_valid(&raw, width, height).then_some(raw)
        });
        if let Some(raw) = cached {
            guard.call0()?;
            return Ok((PyBytes::new(py, &raw).into_any(), complete));
        }
        let none = py.None().into_bound(py);
        let raw = previews.getattr("request")?.call((identity, 0), Some(&pyhelp::kwargs(py, &[("priority", 1i64.into_pyobject(py)?.into_any())])?))?;
        let slots = slf.getattr("slots")?;
        if raw.is_none() || !slots.call_method("acquire", (), Some(&pyhelp::kwargs(py, &[("blocking", false.into_pyobject(py)?.to_owned().into_any())])?))?.is_truthy()? {
            return Ok((none, complete));
        }
        let temporary = target.with_extension(format!("partial-{}", pyhelp::token_hex(py, 8)?));
        let result = (|| -> PyResult<Bound<'py, PyAny>> {
            guard.call0()?;
            let image = downsample_page(py, raw.extract()?, cw, ch, dark)?;
            guard.call0()?;
            let bytes = image.as_bytes();
            if bytes.len() > MAX_THUMB_BYTES || !px::png_valid(bytes, width, height) {
                return Err(xarea::store_error(py, "thumbnail_unavailable", "Not önizlemesi hazırlanamadı.", 422));
            }
            py.detach(|| -> std::io::Result<()> {
                let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW).open(&temporary)?;
                file.write_all(bytes)?;
                file.sync_all()
            })?;
            guard.call0()?;
            std::fs::rename(&temporary, &target)?;
            Ok(image.into_any())
        })();
        let _ = std::fs::remove_file(&temporary);
        slots.call_method0("release")?;
        Ok((result?, complete))
    }
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(thumbnail_size, m)?)?;
    m.add_function(wrap_pyfunction!(downsample_page, m)?)?;
    m.add_class::<ThumbnailManager>()?;
    rs_common::export(m, &["thumbnail_size", "downsample_page", "ThumbnailManager"])
}
