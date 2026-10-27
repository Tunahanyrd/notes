//! Replacement for the Python module `ink_coordinates`.
use crate::core::coords;
use crate::core::num::Val;
use crate::core::InkErr;
#[cfg(feature = "python")]
use crate::pyglue::{bytes_of, err, is_inst, record_frames, val, IntoPy};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyTuple};

#[cfg(feature = "python")]
fn require(py: Python<'_>, record: &Bound<'_, PyAny>, msg: &str) -> PyResult<()> {
    if !is_inst(record, "StrokeRecord")? {
        return Err(err(py, InkErr::Ink(msg.into())));
    }
    Ok(())
}

#[cfg(feature = "python")]
#[pyfunction]
fn stroke_owner_extent<'py>(py: Python<'py>, record: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    require(py, record, "stroke record required")?;
    let frames = record_frames(record, 1)?;
    let (w, h) = coords::stroke_owner_extent(&frames[0]).py(py)?;
    PyTuple::new(py, [w, h])
}

#[cfg(feature = "python")]
fn precheck(py: Python<'_>, width: Val, height: Val, orientation: Val) -> PyResult<()> {
    if !matches!(orientation, Val::Int(0)) {
        return Err(err(py, InkErr::Ink("page orientation conversion unsupported".into())));
    }
    for v in [width, height] {
        if !matches!(v, Val::Int(i) if (1..=32768).contains(&i)) {
            return Err(err(py, InkErr::Ink("invalid page coordinate extent".into())));
        }
    }
    Ok(())
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (record, *, width, height, orientation=None))]
fn page_coordinate_scale<'py>(py: Python<'py>, record: &Bound<'py, PyAny>, width: &Bound<'py, PyAny>,
                              height: &Bound<'py, PyAny>, orientation: Option<&Bound<'py, PyAny>>) -> PyResult<f64> {
    let (w, h) = (val(width), val(height));
    let o = orientation.map(val).unwrap_or(Val::Int(0));
    precheck(py, w, h, o)?;
    require(py, record, "stroke record required")?;
    let frames = record_frames(record, 1)?;
    coords::page_coordinate_scale(&frames[0], w, h, o).py(py)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (record, *, width, height))]
fn rebase_style_bytes<'py>(py: Python<'py>, record: &Bound<'py, PyAny>, width: &Bound<'py, PyAny>,
                           height: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    require(py, record, "validated stroke record required")?;
    let frames = record_frames(record, 2)?;
    crate::core::binary::read_stroke_style(&frames[1]).py(py)?;
    let (w, h) = (val(width), val(height));
    precheck(py, w, h, Val::Int(0))?;
    let out = coords::rebase_style_bytes(&frames[0], &frames[1], w, h).py(py)?;
    Ok(PyBytes::new(py, &out))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (record, data, *, color_argb, pen_size))]
fn override_style_bytes<'py>(py: Python<'py>, record: &Bound<'py, PyAny>, data: &Bound<'py, PyAny>,
                             color_argb: &Bound<'py, PyAny>, pen_size: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let (c, p) = (val(color_argb), val(pen_size));
    coords::check_override_args(c, p).py(py)?;
    let frames = record_frames(record, 2)?;
    let mask = crate::core::binary::mask64(&frames[1].fields).0;
    if (mask >> 2) & 1 == 0 || (mask >> 3) & 1 == 0 {
        return Err(err(py, InkErr::Ink("template lacks colour or size field".into())));
    }
    let data = bytes_of(data)?;
    Ok(PyBytes::new(py, &coords::override_style_bytes(&frames[1], &data, c, p).py(py)?))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(stroke_owner_extent, m)?)?;
    m.add_function(wrap_pyfunction!(page_coordinate_scale, m)?)?;
    m.add_function(wrap_pyfunction!(rebase_style_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(override_style_bytes, m)?)?;
    rs_common::export(m, &["stroke_owner_extent", "page_coordinate_scale", "rebase_style_bytes", "override_style_bytes"])
}
