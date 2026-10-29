//! Port of `samsung_notes/preferences.py`: device preferences live beside the
//! library, independent of HTTP port/origin.
use crate::db::{fail, Result};
#[cfg(feature = "python")]
use crate::pyconv::{err_to_py, to_py, to_val};
use crate::val::{py_eq, Val};
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub const KEY: &str = "ui_preferences_v1";

pub fn default_preferences() -> Val {
    Val::dict(vec![
        ("version", Val::Int(1)),
        ("galleryMode", Val::s("grid")),
        ("sortAscending", Val::Bool(true)),
        ("toolsOpen", Val::Bool(true)),
        ("defaultZoom", Val::Int(100)),
        ("theme", Val::s("system")),
        ("darkPages", Val::Bool(false)),
    ])
}

pub fn validate_preferences(value: &Val) -> Result<Val> {
    let defaults = default_preferences();
    let ok = match value {
        Val::Dict(items) => {
            let mut keys: Vec<&str> = items.iter().map(|(k, _)| k.as_str()).collect();
            let mut want = defaults.keys();
            // Older device records predate these optional fields.
            if value.get("theme").is_none() { want.retain(|k| *k != "theme"); }
            if value.get("darkPages").is_none() { want.retain(|k| *k != "darkPages"); }
            keys.sort();
            want.sort();
            keys == want
                && matches!(value.get("version"), Some(Val::Int(1)))
                && ["grid", "list"].iter().any(|m| py_eq(&value.get_or_null("galleryMode"), &Val::s(*m)))
                && matches!(value.get("sortAscending"), Some(Val::Bool(_)))
                && matches!(value.get("toolsOpen"), Some(Val::Bool(_)))
                && matches!(value.get("defaultZoom"), Some(Val::Int(75 | 100 | 125 | 150)))
                && value.get("theme").map_or(true, |v| ["system", "light", "dark"].iter().any(|m| py_eq(v, &Val::s(*m))))
                && value.get("darkPages").map_or(true, |v| matches!(v, Val::Bool(_)))
        }
        _ => false,
    };
    if !ok {
        return fail("invalid_preferences", "Görünüm tercihleri geçersiz.", 400);
    }
    let mut normalized = value.clone();
    if let Val::Dict(items) = &mut normalized {
        for key in ["theme", "darkPages"] {
            if value.get(key).is_none() { items.push((key.to_string(), defaults.get(key).unwrap().clone())); }
        }
    }
    Ok(normalized)
}

#[cfg(feature = "python")]
#[pyfunction(name = "validate_preferences")]
fn py_validate_preferences(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let v = to_val(value)?;
    let r = validate_preferences(&v).map_err(|e| err_to_py(py, e))?;
    to_py(py, &r)
}

#[cfg(feature = "python")]
#[pyfunction]
fn get_preferences(py: Python<'_>, store: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let default = to_py(py, &default_preferences())?;
    let value = store.call_method1("meta", (KEY, default))?;
    py_validate_preferences(py, &value)
}

#[cfg(feature = "python")]
#[pyfunction]
fn set_preferences(py: Python<'_>, store: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let value = py_validate_preferences(py, value)?;
    store.call_method1("set_meta", (KEY, value.clone_ref(py)))?;
    Ok(value)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(py_validate_preferences, m)?)?;
    m.add_function(wrap_pyfunction!(get_preferences, m)?)?;
    m.add_function(wrap_pyfunction!(set_preferences, m)?)?;
    rs_common::export(m, &["validate_preferences", "get_preferences", "set_preferences"])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn theme_preferences_validate_and_migrate() {
        let defaults = default_preferences();
        assert_eq!(validate_preferences(&defaults).unwrap(), defaults);
        let mut old = defaults.clone();
        if let Val::Dict(items) = &mut old { items.retain(|(key, _)| key != "theme" && key != "darkPages"); }
        assert_eq!(validate_preferences(&old).unwrap(), defaults);
        let mut bad = defaults.clone();
        if let Val::Dict(items) = &mut bad { items.iter_mut().find(|(key, _)| key == "theme").unwrap().1 = Val::s("purple"); }
        assert!(validate_preferences(&bad).is_err());
        let mut bad = defaults.clone();
        if let Val::Dict(items) = &mut bad { items.iter_mut().find(|(key, _)| key == "darkPages").unwrap().1 = Val::Int(1); }
        assert!(validate_preferences(&bad).is_err());
    }
}
