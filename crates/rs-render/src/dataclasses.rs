//! Record types built as real Python dataclasses (`dataclasses.make_dataclass`)
//! so `dataclasses.replace`, equality, hashing and repr behave exactly like
//! the Python definitions. They hold data only; the logic is Rust.
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::sync::PyOnceLock;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyList, PyTuple, PyType};

#[cfg(feature = "python")]
static PDF_BACKGROUND: PyOnceLock<Py<PyType>> = PyOnceLock::new();
#[cfg(feature = "python")]
static INK_GEOMETRY: PyOnceLock<Py<PyType>> = PyOnceLock::new();
#[cfg(feature = "python")]
static ENTRY: PyOnceLock<Py<PyType>> = PyOnceLock::new();

/// (name, type, field kwargs)
type Spec<'a> = (&'a str, &'a str, &'a [(&'a str, FieldDefault)]);

#[derive(Clone, Copy)]
enum FieldDefault {
    NoRepr,
    NoneNoRepr,
    False,
}

#[cfg(feature = "python")]
fn build<'py>(py: Python<'py>, name: &str, module: &str, fields: &[Spec], frozen: bool, namespace: Bound<'py, PyDict>) -> PyResult<Bound<'py, PyType>> {
    let dc = py.import("dataclasses")?;
    let builtins = py.import("builtins")?;
    let field = dc.getattr("field")?;
    let mut items = Vec::new();
    for (fname, ftype, options) in fields {
        let annotation = builtins.getattr(*ftype)?;
        let kwargs = PyDict::new(py);
        for (_, option) in options.iter() {
            match option {
                FieldDefault::NoRepr => kwargs.set_item("repr", false)?,
                FieldDefault::NoneNoRepr => {
                    kwargs.set_item("repr", false)?;
                    kwargs.set_item("default", py.None())?;
                }
                FieldDefault::False => kwargs.set_item("default", false)?,
            }
        }
        if kwargs.is_empty() {
            items.push(PyTuple::new(py, [PyString(fname).obj(py), annotation])?.into_any());
        } else {
            items.push(PyTuple::new(py, [PyString(fname).obj(py), annotation, field.call((), Some(&kwargs))?])?.into_any());
        }
    }
    namespace.set_item("__module__", module)?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("namespace", namespace)?;
    kwargs.set_item("frozen", frozen)?;
    let class = dc.getattr("make_dataclass")?.call((name, PyList::new(py, items)?), Some(&kwargs))?;
    class.setattr("__module__", module)?;
    Ok(class.cast_into::<PyType>()?)
}

struct PyString<'a>(&'a str);
impl PyString<'_> {
    #[cfg(feature = "python")]
    fn obj<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        pyo3::types::PyString::new(py, self.0).into_any()
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn _pdf_is_dark_mode(_self: &Bound<'_, PyAny>) -> bool {
    false
}

#[cfg(feature = "python")]
/// PDF background only; native canvas objects are separate render layers.
#[pyfunction]
fn _pdf_complete(slf: &Bound<'_, PyAny>) -> PyResult<bool> {
    Ok(slf.getattr("annotation_objects")?.eq(0)? || !slf.getattr("overlay")?.is_none())
}

#[cfg(feature = "python")]
pub fn pdf_background(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    PDF_BACKGROUND.get_or_try_init(py, || -> PyResult<Py<PyType>> {
        let property = py.import("builtins")?.getattr("property")?;
        let namespace = PyDict::new(py);
        let helpers = pyo3::types::PyModule::new(py, "_pdf_background_helpers")?;
        namespace.set_item("is_dark_mode", property.call1((wrap_pyfunction!(_pdf_is_dark_mode, &helpers)?,))?)?;
        namespace.set_item("complete", property.call1((wrap_pyfunction!(_pdf_complete, &helpers)?,))?)?;
        let fields: [Spec; 9] = [("page_index", "int", &[]), ("pdf_page_index", "int", &[]), ("width", "int", &[]),
            ("height", "int", &[]), ("rect", "tuple", &[]), ("annotation_objects", "int", &[]),
            ("sha256", "str", &[("", FieldDefault::NoRepr)]), ("download_url", "object", &[("", FieldDefault::NoRepr)]),
            ("overlay", "object", &[("", FieldDefault::NoneNoRepr)])];
        Ok(build(py, "PdfBackground", "samsung_notes.pdf_background", &fields, true, namespace)?.unbind())
    }).map(|t| t.bind(py))
}

#[cfg(feature = "python")]
pub fn ink_geometry_preview(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    INK_GEOMETRY.get_or_try_init(py, || -> PyResult<Py<PyType>> {
        let fields: [Spec; 5] = [("page_index", "int", &[]), ("width", "int", &[]), ("height", "int", &[]), ("sha256", "str", &[]),
            ("is_dark_mode", "bool", &[("", FieldDefault::False)])];
        Ok(build(py, "InkGeometryPreview", "samsung_notes.previews", &fields, true, PyDict::new(py))?.unbind())
    }).map(|t| t.bind(py))
}

#[cfg(feature = "python")]
pub fn entry(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    ENTRY.get_or_try_init(py, || -> PyResult<Py<PyType>> {
        let fields: [Spec; 3] = [("source", "tuple", &[]), ("pages", "tuple", &[]), ("candidates", "dict", &[])];
        Ok(build(py, "_Entry", "samsung_notes.previews", &fields, false, PyDict::new(py))?.unbind())
    }).map(|t| t.bind(py))
}

#[cfg(feature = "python")]
pub fn is_pdf(value: &Bound<'_, PyAny>) -> bool {
    pdf_background(value.py()).and_then(|t| value.is_instance(t)).unwrap_or(false)
}

#[cfg(feature = "python")]
pub fn is_ink(value: &Bound<'_, PyAny>) -> bool {
    ink_geometry_preview(value.py()).and_then(|t| value.is_instance(t)).unwrap_or(false)
}

#[cfg(feature = "python")]
/// dataclasses.replace(value, **{name: new})
pub fn replace<'py>(value: &Bound<'py, PyAny>, name: &str, new: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    let kwargs = PyDict::new(py);
    kwargs.set_item(name, new)?;
    py.import("dataclasses")?.getattr("replace")?.call((value,), Some(&kwargs))
}
