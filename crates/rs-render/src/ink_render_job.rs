//! Replacement for the Python module `ink_render_job`, plus `_serve`, the
//! streaming preparation child used by ink_render (one decode per note).
#[cfg(feature = "python")]
use crate::ink_render::{_prepare_pages, PageInput};
use crate::sandbox;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::PyBytes;
use std::io::Write;

const LIMITS: sandbox::Limits = sandbox::Limits { address_space: 3 * 1024 * 1024 * 1024, cpu_seconds: 170, file_size: None };

#[cfg(feature = "python")]
fn read_snapshot<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    let raw = py.import("sys")?.getattr("stdin")?.getattr("buffer")?.call_method1("read", (256usize * 1024 * 1024,))?;
    py.import("json")?.getattr("loads")?.call1((raw,))
}

#[cfg(feature = "python")]
fn inventory_json(py: Python<'_>, snapshot: &Bound<'_, PyAny>) -> PyResult<String> {
    let pages = match _prepare_pages(py, snapshot, None, false) {
        Ok(p) => p,
        Err(e) if e.is_instance_of::<pyo3::exceptions::PyValueError>(py) || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
            || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py) || e.is_instance_of::<pyo3::exceptions::PyOverflowError>(py)
            || e.is_instance_of::<pyo3::exceptions::PySyntaxError>(py) => pyo3::types::PyTuple::empty(py),
        Err(e) => return Err(e),
    };
    let items: Vec<String> = pages.iter().map(|p| {
        let p = p.cast::<PageInput>().unwrap().get();
        format!("{{\"index\": {}, \"width\": {}, \"height\": {}, \"darkMode\": {}}}", p.index(), p.width(), p.height(),
                if p.dark_mode() { "true" } else { "false" })
    }).collect();
    Ok(format!("[{}]", items.join(", ")))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn main(py: Python<'_>) -> PyResult<i32> {
    let argv: Vec<String> = py.import("sys")?.getattr("argv")?.extract()?;
    let int = py.import("builtins")?.getattr("int")?;
    if !(argv.len() == 3 || argv.len() == 4) || !sandbox::bind_parent(int.call1((&argv[2],))?.extract()?) {
        return Ok(71);
    }
    let mode = argv[1].as_str();
    if (mode != "inventory" && mode != "page") || (mode == "page") != (argv.len() == 4) {
        return Ok(65);
    }
    sandbox::apply_limits(&LIMITS, true)?;
    let snapshot = read_snapshot(py)?;
    if mode == "inventory" {
        let text = inventory_json(py, &snapshot)?;
        py.import("sys")?.getattr("stdout")?.call_method1("write", (text,))?;
        return Ok(0);
    }
    let index: i64 = int.call1((&argv[3],))?.extract()?;
    let pages = _prepare_pages(py, &snapshot, Some(index), true)?;
    for p in pages.iter() {
        let page = p.cast::<PageInput>()?.get();
        if page.index() == index {
            py.import("sys")?.getattr("stdout")?.getattr("buffer")?.call_method1("write", (page.body_bytes(py),))?;
            return Ok(0);
        }
    }
    Ok(66)
}

fn frame(out: &mut impl Write, index: i64, body: &[u8]) -> std::io::Result<()> {
    out.write_all(&(index as i32).to_le_bytes())?;
    out.write_all(&(body.len() as u32).to_le_bytes())?;
    out.write_all(body)?;
    out.flush()
}

#[cfg(feature = "python")]
/// Child entry (argv: render parent_pid page). Writes the inventory frame,
/// the wanted page, then every other admitted page; an empty body marks a
/// page that could not be prepared. Limits and parent binding were applied
/// before exec by the parent.
#[pyfunction]
pub fn _serve(py: Python<'_>) -> PyResult<i32> {
    let argv: Vec<String> = py.import("sys")?.getattr("argv")?.extract()?;
    if argv.len() != 4 || argv[1] != "render" {
        return Ok(65);
    }
    let wanted: i64 = argv[3].parse().map_err(|_| pyo3::exceptions::PyValueError::new_err("page"))?;
    let snapshot = read_snapshot(py)?;
    let mut out = std::io::stdout().lock();
    let inventory = inventory_json(py, &snapshot)?;
    out.write_all(&(inventory.len() as u32).to_le_bytes())?;
    out.write_all(inventory.as_bytes())?;
    out.flush()?;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&inventory).unwrap_or_default();
    let mut order: Vec<i64> = parsed.iter().filter_map(|p| p.get("index").and_then(|i| i.as_i64())).collect();
    if let Some(at) = order.iter().position(|i| *i == wanted) {
        order.remove(at);
        order.insert(0, wanted);
    } else {
        return Ok(0);
    }
    for index in order {
        let body = match _prepare_pages(py, &snapshot, Some(index), true) {
            Ok(pages) => pages.iter().find_map(|p| {
                let page = p.cast::<PageInput>().ok()?.get();
                (page.index() == index).then(|| page.body_bytes(py).as_bytes().to_vec())
            }).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if frame(&mut out, index, &body).is_err() {
            break;
        }
    }
    let _ = PyBytes::new(py, b"");
    Ok(0)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(main, m)?)?;
    m.add_function(wrap_pyfunction!(_serve, m)?)?;
    rs_common::export(m, &["main", "_serve"])
}
