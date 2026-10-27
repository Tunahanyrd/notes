//! Small PyO3 helpers shared by the module wrappers.
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyTuple};

#[cfg(feature = "python")]
/// A same-area module global, looked up at call time so tests that patch
/// module attributes (and the override itself) are honoured.
pub fn global<'py>(py: Python<'py>, module: &str, name: &str) -> PyResult<Bound<'py, PyAny>> {
    py.import(module)?.getattr(name)
}

#[cfg(feature = "python")]
pub fn bytes<'py>(py: Python<'py>, data: &[u8]) -> Bound<'py, PyBytes> {
    PyBytes::new(py, data)
}

#[cfg(feature = "python")]
/// `with cm as value:` around `body`, passing exception details to __exit__.
pub fn with_cm<'py, T>(cm: &Bound<'py, PyAny>, body: impl FnOnce(&Bound<'py, PyAny>) -> PyResult<T>) -> PyResult<T> {
    let py = cm.py();
    let entered = cm.call_method0("__enter__")?;
    match body(&entered) {
        Ok(value) => {
            cm.call_method1("__exit__", (py.None(), py.None(), py.None()))?;
            Ok(value)
        }
        Err(error) => {
            let traceback = error.traceback(py).map(|t| t.into_any().unbind()).unwrap_or_else(|| py.None());
            cm.call_method1("__exit__", (error.get_type(py), error.value(py), traceback))?;
            Err(error)
        }
    }
}

#[cfg(feature = "python")]
/// `with lock:` around `body`.
pub fn with_lock<'py, T>(lock: &Bound<'py, PyAny>, body: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    with_cm(lock, |_| body())
}

#[cfg(feature = "python")]
pub fn kwargs<'py>(py: Python<'py>, items: &[(&str, Bound<'py, PyAny>)]) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (key, value) in items {
        dict.set_item(key, value)?;
    }
    Ok(dict)
}

#[cfg(feature = "python")]
pub fn tuple<'py>(py: Python<'py>, items: Vec<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(py, items)
}

#[cfg(feature = "python")]
/// `type(value) is int`, like the Python contract checks.
pub fn exact_int(value: &Bound<'_, PyAny>) -> Option<i64> {
    if value.is_exact_instance_of::<pyo3::types::PyInt>() {
        value.extract::<i64>().ok()
    } else {
        None
    }
}

#[cfg(feature = "python")]
/// Python truthiness.
pub fn truthy(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    value.is_truthy()
}

#[cfg(feature = "python")]
pub fn path<'py>(py: Python<'py>, value: impl IntoPyObject<'py>) -> PyResult<Bound<'py, PyAny>> {
    py.import("pathlib")?.getattr("Path")?.call1((value,))
}

#[cfg(feature = "python")]
pub fn monotonic(py: Python<'_>) -> PyResult<f64> {
    py.import("time")?.getattr("monotonic")?.call0()?.extract()
}

#[cfg(feature = "python")]
pub fn token_hex(py: Python<'_>, n: usize) -> PyResult<String> {
    py.import("secrets")?.getattr("token_hex")?.call1((n,))?.extract()
}
