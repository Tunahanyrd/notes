//! Port of `samsung_notes/shortcuts.py`: versioned device shortcuts,
//! independent of browser origin and Cloud account. Schema 2 covers the
//! Windows Samsung Notes shortcuts whose action exists in this client;
//! schema 1 is read from its old key and accepted on PUT (upgraded).
use crate::db::{fail, Result};
#[cfg(feature = "python")]
use crate::pyconv::{err_to_py, to_py, to_val};
use crate::val::Val;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use std::collections::HashSet;

pub const LEGACY_KEY: &str = "keyboard_shortcuts_v1";
pub const KEY: &str = "keyboard_shortcuts_v2";

const LEGACY: [(&str, &str); 3] = [("newNote", "Ctrl+N"), ("searchNotes", "Ctrl+K"), ("saveNote", "Ctrl+S")];
const DEFAULTS: [(&str, &str); 24] = [
    ("newNote", "Ctrl+N"),
    ("searchNotes", "Ctrl+K"),
    ("saveNote", "Ctrl+S"),
    ("zoomIn", "Ctrl+Plus"),
    ("zoomOut", "Ctrl+Minus"),
    ("readingMode", "Alt+Enter"),
    ("fullScreen", "Alt+F"),
    ("penSettings", "Ctrl+T"),
    ("pageSettings", "Alt+P"),
    ("printNote", "Ctrl+Shift+P"),
    ("bold", "Ctrl+B"),
    ("italic", "Ctrl+I"),
    ("underline", "Ctrl+U"),
    ("strikethrough", "Ctrl+Slash"),
    ("alignLeft", "Ctrl+L"),
    ("alignCenter", "Ctrl+E"),
    ("alignRight", "Ctrl+R"),
    ("numberedList", "Ctrl+Shift+7"),
    ("bulletList", "Ctrl+Shift+8"),
    ("checklist", "Ctrl+Shift+C"),
    ("fontSmaller", "Ctrl+BracketLeft"),
    ("fontLarger", "Ctrl+BracketRight"),
    ("indent", "Ctrl+M"),
    ("outdent", "Ctrl+Shift+M"),
];
const RESERVED_CHORDS: [&str; 13] = [
    "Ctrl+A", "Ctrl+C", "Ctrl+V", "Ctrl+X", "Ctrl+Z", "Ctrl+Y", "Ctrl+Shift+Z", "Ctrl+Q", "Ctrl+W", "Ctrl+Shift+N",
    "Ctrl+Shift+K", "Ctrl+Shift+I", "Ctrl+Shift+V",
];
const NAMED_KEYS: [&str; 10] = ["Plus", "Minus", "Slash", "BracketLeft", "BracketRight", "Comma", "Period", "Quote", "Semicolon", "Enter"];
const LEGACY_RESERVED_KEYS: &str = "ABCDEHIJLOPQRTUVWXYZ0123456789";
const LEGACY_RESERVED_CHORDS: [&str; 2] = ["Ctrl+Shift+N", "Ctrl+Shift+K"];

fn pairs(items: &[(&str, &str)]) -> Val {
    Val::Dict(items.iter().map(|(k, v)| (k.to_string(), Val::s(*v))).collect())
}

/// Schema 2 default bindings.
pub fn default_bindings() -> Val {
    pairs(&DEFAULTS)
}

pub fn default_shortcuts() -> Val {
    Val::dict(vec![("schemaVersion", Val::Int(2)), ("bindings", default_bindings())])
}

fn invalid<T>() -> Result<T> {
    fail("invalid_shortcuts", "Kısayol ayarları geçersiz.", 400)
}
fn bad_chord<T>() -> Result<T> {
    fail("invalid_shortcut", "Bu tuş birleşimi kısayol olarak kullanılamaz.", 400)
}
fn conflict<T>() -> Result<T> {
    fail("shortcut_conflict", "Her işlem için farklı bir kısayol seçin.", 400)
}

fn same_keys(v: &Val, want: &[&str]) -> bool {
    let keys: HashSet<&str> = v.keys().into_iter().collect();
    keys.len() == want.len() && want.iter().all(|k| keys.contains(k))
}

fn shape<'a>(value: &'a Val, version: i64, names: &[&str]) -> Result<&'a Vec<(String, Val)>> {
    let ok = matches!(value, Val::Dict(_))
        && same_keys(value, &["schemaVersion", "bindings"])
        && matches!(value.get("schemaVersion"), Some(Val::Int(n)) if *n == version)
        && matches!(value.get("bindings"), Some(Val::Dict(_)))
        && same_keys(value.get("bindings").unwrap(), names);
    match (ok, value.get("bindings")) {
        (true, Some(Val::Dict(items))) => Ok(items),
        _ => invalid(),
    }
}

/// `(?:Ctrl\+(?:Alt\+)?|Alt\+)(?:Shift\+)?(?:[A-Z0-9]|Plus|...|Enter)\Z`
fn chord_ok(chord: &str) -> bool {
    let rest = if let Some(r) = chord.strip_prefix("Ctrl+") {
        r.strip_prefix("Alt+").unwrap_or(r)
    } else if let Some(r) = chord.strip_prefix("Alt+") {
        r
    } else {
        return false;
    };
    let key = rest.strip_prefix("Shift+").unwrap_or(rest);
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.is_ascii_uppercase() || c.is_ascii_digit(),
        _ => NAMED_KEYS.contains(&key),
    }
}

/// `Ctrl\+(?:Shift\+)?([A-Z0-9])\Z` -> captured key
fn legacy_key(chord: &str) -> Option<char> {
    let rest = chord.strip_prefix("Ctrl+")?;
    let rest = rest.strip_prefix("Shift+").unwrap_or(rest);
    let mut chars = rest.chars();
    let c = chars.next()?;
    (chars.next().is_none() && (c.is_ascii_uppercase() || c.is_ascii_digit())).then_some(c)
}

fn legacy(value: &Val) -> Result<Vec<(String, String)>> {
    let names: Vec<&str> = LEGACY.iter().map(|(k, _)| *k).collect();
    let bindings = shape(value, 1, &names)?;
    let mut out = Vec::new();
    for (name, chord) in bindings {
        let valid = match chord {
            Val::Str(s) => match legacy_key(s) {
                Some(c) => !LEGACY_RESERVED_KEYS.contains(c) && !LEGACY_RESERVED_CHORDS.contains(&s.as_str()),
                None => false,
            },
            _ => false,
        };
        if !valid {
            return bad_chord();
        }
        out.push((name.clone(), chord.as_str().unwrap().to_string()));
    }
    let seen: HashSet<&str> = out.iter().map(|(_, c)| c.as_str()).collect();
    if seen.len() != out.len() {
        return conflict();
    }
    Ok(out)
}

/// Schema 2. An empty string leaves an action without a shortcut.
pub fn validate_shortcuts(value: &Val) -> Result<Val> {
    let names: Vec<&str> = DEFAULTS.iter().map(|(k, _)| *k).collect();
    let bindings = shape(value, 2, &names)?;
    let mut used = Vec::new();
    for (_, chord) in bindings {
        match chord {
            Val::Str(s) if s.is_empty() => {}
            Val::Str(s) if chord_ok(s) && !RESERVED_CHORDS.contains(&s.as_str()) => used.push(s.as_str()),
            _ => return bad_chord(),
        }
    }
    if used.iter().collect::<HashSet<_>>().len() != used.len() {
        return conflict();
    }
    let ordered = names.iter().map(|name| (name.to_string(), value.get("bindings").unwrap().get(name).unwrap().clone())).collect();
    Ok(Val::dict(vec![("schemaVersion", Val::Int(2)), ("bindings", Val::Dict(ordered))]))
}

fn upgrade(base: &Val, legacy: &[(String, String)]) -> Result<Val> {
    let taken: HashSet<&str> = legacy.iter().map(|(_, c)| c.as_str()).collect();
    let mut items = Vec::new();
    for (name, _) in DEFAULTS.iter() {
        let value = match legacy.iter().find(|(k, _)| k == name) {
            Some((_, chord)) => Val::s(chord.clone()),
            None => match base.get(name) {
                Some(Val::Str(s)) if taken.contains(s.as_str()) => Val::s(""),
                Some(other) => other.clone(),
                None => Val::Null,
            },
        };
        items.push((name.to_string(), value));
    }
    validate_shortcuts(&Val::dict(vec![("schemaVersion", Val::Int(2)), ("bindings", Val::Dict(items))]))
}

/// `get_shortcuts` on the stored values of `KEY` and `LEGACY_KEY`.
pub fn load(current: Option<Val>, legacy_value: Option<Val>) -> Result<Val> {
    if let Some(value) = current.filter(|v| !v.is_null()) {
        return validate_shortcuts(&value);
    }
    match legacy_value.filter(|v| !v.is_null()) {
        None => Ok(default_shortcuts()),
        Some(value) => upgrade(&default_bindings(), &legacy(&value)?),
    }
}

/// `set_shortcuts` without the write: the document to store under `KEY`.
/// `current` yields what `get_shortcuts` returns now (only read for schema 1).
pub fn prepare(value: &Val, current: impl FnOnce() -> Result<Val>) -> Result<Val> {
    if matches!(value, Val::Dict(_)) && matches!(value.get("schemaVersion"), Some(Val::Int(1))) {
        let old = legacy(value)?;
        let now = current()?;
        let base = now.get("bindings").cloned().unwrap_or(Val::Null);
        return upgrade(&base, &old);
    }
    validate_shortcuts(value)
}

#[cfg(feature = "python")]
#[pyfunction(name = "validate_shortcuts")]
fn py_validate_shortcuts(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let v = to_val(value)?;
    let r = validate_shortcuts(&v).map_err(|e| err_to_py(py, e))?;
    to_py(py, &r)
}

#[cfg(feature = "python")]
fn stored(store: &Bound<'_, PyAny>, key: &str) -> PyResult<Option<Val>> {
    let value = store.call_method1("meta", (key, store.py().None()))?;
    if value.is_none() {
        Ok(None)
    } else {
        Ok(Some(to_val(&value)?))
    }
}

#[cfg(feature = "python")]
fn py_load(py: Python<'_>, store: &Bound<'_, PyAny>) -> PyResult<Val> {
    let current = stored(store, KEY)?;
    let legacy_value = if current.is_none() { stored(store, LEGACY_KEY)? } else { None };
    load(current, legacy_value).map_err(|e| err_to_py(py, e))
}

#[cfg(feature = "python")]
#[pyfunction]
fn get_shortcuts(py: Python<'_>, store: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let value = py_load(py, store)?;
    to_py(py, &value)
}

#[cfg(feature = "python")]
#[pyfunction]
fn set_shortcuts(py: Python<'_>, store: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let v = to_val(value)?;
    let mut read_error: Option<PyErr> = None;
    let prepared = prepare(&v, || match py_load(py, store) {
        Ok(value) => Ok(value),
        Err(error) => {
            read_error = Some(error);
            fail("internal", "", 500)
        }
    });
    let prepared = match (prepared, read_error) {
        (_, Some(error)) => return Err(error),
        (Ok(value), None) => value,
        (Err(e), None) => return Err(err_to_py(py, e)),
    };
    let out = to_py(py, &prepared)?;
    store.call_method1("set_meta", (KEY, out.clone_ref(py)))?;
    Ok(out)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(py_validate_shortcuts, m)?)?;
    m.add_function(wrap_pyfunction!(get_shortcuts, m)?)?;
    m.add_function(wrap_pyfunction!(set_shortcuts, m)?)?;
    rs_common::export(m, &["validate_shortcuts", "get_shortcuts", "set_shortcuts"])
}
