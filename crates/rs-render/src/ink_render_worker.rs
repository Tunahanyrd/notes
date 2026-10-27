//! Replacement for the Python module `ink_render_worker`.
//! (ink_render applies the same limits and sandbox directly in the child.)
use crate::sandbox;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use std::path::PathBuf;

#[cfg(feature = "python")]
#[pyfunction]
pub fn main(py: Python<'_>) -> PyResult<i32> {
    let argv: Vec<String> = py.import("sys")?.getattr("argv")?.extract()?;
    let int = py.import("builtins")?.getattr("int")?;
    if argv.len() != 4 || !sandbox::bind_parent(int.call1((&argv[3],))?.extract()?) {
        return Ok(71);
    }
    let mode = argv[1].as_str();
    let binary: PathBuf = py.import("pathlib")?.getattr("Path")?.call1((&argv[2],))?.call_method0("resolve")?
        .call_method0("__str__")?.extract::<String>()?.into();
    if (mode != "svg" && mode != "png") || !binary.is_file() {
        return Ok(65);
    }
    if mode == "png" && Some(binary.clone()) != std::fs::canonicalize(sandbox::rsvg_path()).ok() {
        return Ok(65);
    }
    sandbox::apply_limits(&sandbox::INK_LIMITS, true)?;
    sandbox::pin_lowest_cpu()?;
    let args = sandbox::ink_args(&binary.to_string_lossy(), mode == "png");
    Err(PyErr::from(sandbox::execv(&args)))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(main, m)?)?;
    rs_common::export(m, &["main"])
}
