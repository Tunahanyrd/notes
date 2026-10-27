//! PyO3 glue between `pyjson::JValue` and Python objects, with `json.loads` /
//! `json.dumps(ensure_ascii=False, allow_nan=False)` semantics.
#[cfg(feature = "python")]
use pyo3::exceptions::{PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};

use crate::pyjson::{self, JStr, JValue};

#[cfg(feature = "python")]
pub fn jstr_to_py<'py>(py: Python<'py>, s: &JStr) -> PyResult<Bound<'py, PyAny>> {
    match s {
        JStr::Utf8(s) => Ok(PyString::new(py, s).into_any()),
        JStr::Wide(units) => {
            let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
            PyBytes::new(py, &bytes).call_method1("decode", ("utf-16-le", "surrogatepass"))
        }
    }
}

#[cfg(feature = "python")]
/// JSON value -> Python object exactly as `json.loads` builds it.
pub fn to_py<'py>(py: Python<'py>, value: &JValue) -> PyResult<Bound<'py, PyAny>> {
    Ok(match value {
        JValue::Null => py.None().into_bound(py),
        JValue::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
        JValue::Int(text) => match text.parse::<i64>() {
            Ok(v) => v.into_pyobject(py)?.into_any(),
            Err(_) => py.get_type::<PyInt>().call1((text.as_str(),))?,
        },
        JValue::Float(text) => PyFloat::new(py, text.parse::<f64>().map_err(|_| PyValueError::new_err("invalid float"))?).into_any(),
        JValue::Str(s) => jstr_to_py(py, s)?,
        JValue::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(to_py(py, item)?)?;
            }
            list.into_any()
        }
        JValue::Object(fields) => {
            let dict = PyDict::new(py);
            for (k, v) in fields {
                dict.set_item(jstr_to_py(py, k)?, to_py(py, v)?)?;
            }
            dict.into_any()
        }
    })
}

#[cfg(feature = "python")]
fn float_text(v: f64) -> PyResult<String> {
    if !v.is_finite() {
        return Err(PyValueError::new_err(format!("Out of range float values are not JSON compliant: {}", v)));
    }
    Ok(pyjson::float_repr(v))
}

#[cfg(feature = "python")]
fn key_text(key: &Bound<'_, PyAny>) -> PyResult<JStr> {
    if let Ok(s) = key.cast::<PyString>() {
        return Ok(JStr::Utf8(s.to_str()?.to_string()));
    }
    if key.is_instance_of::<PyBool>() {
        return Ok(JStr::Utf8(if key.is_truthy()? { "true" } else { "false" }.into()));
    }
    if key.is_none() {
        return Ok(JStr::Utf8("null".into()));
    }
    if key.is_instance_of::<PyInt>() {
        return Ok(JStr::Utf8(key.py().get_type::<PyInt>().getattr("__repr__")?.call1((key,))?.extract::<String>()?));
    }
    if key.is_instance_of::<PyFloat>() {
        return Ok(JStr::Utf8(float_text(key.extract::<f64>()?)?));
    }
    Err(PyTypeError::new_err(format!(
        "keys must be str, int, float, bool or None, not {}",
        key.get_type().name()?
    )))
}

#[cfg(feature = "python")]
/// Python object -> JSON value as `json.dumps(..., allow_nan=False)` sees it.
pub fn from_py(obj: &Bound<'_, PyAny>, depth: usize) -> PyResult<JValue> {
    if depth > 5000 {
        return Err(pyo3::exceptions::PyRecursionError::new_err("maximum recursion depth exceeded while encoding a JSON object"));
    }
    if let Ok(s) = obj.cast::<PyString>() {
        // A lone surrogate fails later at `.encode()` in Python; same outcome.
        return Ok(JValue::Str(JStr::Utf8(s.to_str()?.to_string())));
    }
    if obj.is_none() {
        return Ok(JValue::Null);
    }
    if obj.is_instance_of::<PyBool>() {
        return Ok(JValue::Bool(obj.is_truthy()?));
    }
    if obj.is_instance_of::<PyInt>() {
        let text: String = obj.py().get_type::<PyInt>().getattr("__repr__")?.call1((obj,))?.extract()?;
        return Ok(JValue::Int(text));
    }
    if obj.is_instance_of::<PyFloat>() {
        return Ok(JValue::Float(float_text(obj.extract::<f64>()?)?));
    }
    if obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>() {
        let mut items = Vec::new();
        for item in obj.try_iter()? {
            items.push(from_py(&item?, depth + 1)?);
        }
        return Ok(JValue::Array(items));
    }
    if let Ok(dict) = obj.cast::<PyDict>() {
        let mut fields = Vec::with_capacity(dict.len());
        for (k, v) in dict.iter() {
            fields.push((key_text(&k)?, from_py(&v, depth + 1)?));
        }
        return Ok(JValue::Object(fields));
    }
    Err(PyTypeError::new_err(format!("Object of type {} is not JSON serializable", obj.get_type().name()?)))
}

#[cfg(feature = "python")]
/// `json.dumps(obj, ensure_ascii=False, allow_nan=False).encode()`.
pub fn dumps_py(obj: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    let value = from_py(obj, 0)?;
    pyjson::dumps(&value).map_err(|_| PyValueError::new_err("JSON value is not encodable"))
}

#[cfg(feature = "python")]
/// Plain `json.loads(bytes)` into Python objects.
pub fn loads_py<'py>(py: Python<'py>, raw: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    let value = pyjson::loads_lenient(raw).map_err(|_| PyValueError::new_err("invalid JSON document"))?;
    to_py(py, &value)
}
