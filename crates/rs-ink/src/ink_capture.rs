//! Replacement for the Python module `ink_capture`.
use crate::core::capture::{self, NormalizedCapture};
use crate::core::num::Val;
use crate::core::prepare::StyleOverride;
use crate::core::InkErr;
#[cfg(feature = "python")]
use crate::ink_editor::{prepare_py, GeomIn};
#[cfg(feature = "python")]
use crate::pyglue::{cls, err, is_inst, raw_samples, samples_obj, val, IntoPy};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBool, PyBytes, PyString, PyTuple};

#[cfg(feature = "python")]
fn normalize(py: Python<'_>, samples: &Bound<'_, PyAny>, stylus: &Bound<'_, PyAny>) -> PyResult<NormalizedCapture> {
    if !is_inst(samples, "StrokeSamples")? || !stylus.is_exact_instance_of::<PyBool>() {
        return Err(err(py, InkErr::Ink("invalid capture".into())));
    }
    capture::normalize_capture(&raw_samples(samples)?, stylus.is_truthy()?).py(py)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (samples, *, stylus=None))]
fn normalize_capture<'py>(py: Python<'py>, samples: &Bound<'py, PyAny>, stylus: Option<&Bound<'py, PyAny>>)
                          -> PyResult<Bound<'py, PyAny>> {
    let default = PyBool::new(py, false).to_owned().into_any();
    let c = normalize(py, samples, stylus.unwrap_or(&default))?;
    cls(py, "NormalizedCapture")?.call1((samples_obj(py, &c.samples)?, PyBytes::new(py, &c.channels), c.stylus,
                                          c.max_coordinate_error, c.max_pressure_error))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (samples, *, pen_name, pen_size, rotation=None))]
fn basic_pen_drawn_bounds<'py>(py: Python<'py>, samples: &Bound<'py, PyAny>, pen_name: &Bound<'py, PyAny>,
                               pen_size: &Bound<'py, PyAny>, rotation: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyTuple>> {
    let name = pen_name.cast::<PyString>().ok().and_then(|s| s.to_str().ok().map(str::to_string));
    let rot = rotation.map(val).unwrap_or(Val::Float(0.0));
    let (size, rot_v) = (val(pen_size), rot);
    // Arguments are checked in the oracle's order before samples are read.
    let pts = if name.as_deref().is_some_and(|n| capture::BASIC_PENS.contains(&n)) && rot_v.is_num() && rot_v.is_zero()
        && is_inst(samples, "StrokeSamples")?
    {
        Some(raw_samples(samples)?.points)
    } else {
        None
    };
    let b = capture::basic_pen_drawn_bounds(pts.as_deref(), name.as_deref(), size, rot_v).py(py)?;
    PyTuple::new(py, b)
}

#[cfg(feature = "python")]
fn append<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, source: &Bound<'py, PyAny>, new_id: &Bound<'py, PyAny>,
               samples: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>, recipe: u8, style: Option<StyleOverride>)
               -> PyResult<Bound<'py, PyAny>> {
    let no = PyBool::new(py, false).to_owned().into_any();
    let c = normalize(py, samples, &no)?;
    prepare_py(py, snapshot, source, new_id, GeomIn::Capture(&c), now_ms, recipe, style)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (snapshot, source_stroke_id, *, new_stroke_id, samples, now_ms))]
fn prepare_ink_append<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, source_stroke_id: &Bound<'py, PyAny>,
                           new_stroke_id: &Bound<'py, PyAny>, samples: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>)
                           -> PyResult<Bound<'py, PyAny>> {
    append(py, snapshot, source_stroke_id, new_stroke_id, samples, now_ms, 1, None)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (snapshot, source_stroke_id, *, new_stroke_id, samples, now_ms))]
fn prepare_ink_append_v2<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, source_stroke_id: &Bound<'py, PyAny>,
                              new_stroke_id: &Bound<'py, PyAny>, samples: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>)
                              -> PyResult<Bound<'py, PyAny>> {
    append(py, snapshot, source_stroke_id, new_stroke_id, samples, now_ms, 2, None)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (snapshot, source_stroke_id, *, new_stroke_id, samples, now_ms, color_argb, pen_size))]
#[allow(clippy::too_many_arguments)]
fn prepare_ink_append_v3<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, source_stroke_id: &Bound<'py, PyAny>,
                              new_stroke_id: &Bound<'py, PyAny>, samples: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>,
                              color_argb: &Bound<'py, PyAny>, pen_size: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let style = StyleOverride { color_argb: val(color_argb), pen_size: val(pen_size) };
    append(py, snapshot, source_stroke_id, new_stroke_id, samples, now_ms, 3, Some(style))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    crate::pyglue::add_classes(m, &["NormalizedCapture"])?;
    m.add_function(wrap_pyfunction!(normalize_capture, m)?)?;
    m.add_function(wrap_pyfunction!(basic_pen_drawn_bounds, m)?)?;
    m.add_function(wrap_pyfunction!(prepare_ink_append, m)?)?;
    m.add_function(wrap_pyfunction!(prepare_ink_append_v2, m)?)?;
    m.add_function(wrap_pyfunction!(prepare_ink_append_v3, m)?)?;
    rs_common::export(m, &["NormalizedCapture", "normalize_capture", "basic_pen_drawn_bounds", "prepare_ink_append",
                           "prepare_ink_append_v2", "prepare_ink_append_v3"])
}
