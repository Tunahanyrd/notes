//! Replacement for the Python module `pdf_background_worker`.
//! (The app itself no longer starts this module: pdf_background applies the
//! same limits and bwrap sandbox directly in the forked child.)
use crate::sandbox;
#[cfg(feature = "python")]
use pyo3::prelude::*;

#[cfg(feature = "python")]
#[pyfunction]
pub fn main(py: Python<'_>) -> PyResult<i32> {
    let argv: Vec<String> = py.import("sys")?.getattr("argv")?.extract()?;
    let int = py.import("builtins")?.getattr("int")?;
    if argv.len() != 7 || !sandbox::bind_parent(int.call1((&argv[6],))?.extract()?) {
        return Ok(71);
    }
    let page: i64 = int.call1((&argv[3],))?.extract()?;
    let width: i64 = int.call1((&argv[4],))?.extract()?;
    let height: i64 = int.call1((&argv[5],))?.extract()?;
    if !(0..=100_000).contains(&page) || width.min(height) <= 0 || width * height > 4 * 1024 * 1024 {
        return Ok(65);
    }
    sandbox::apply_limits(&sandbox::PDF_LIMITS, true)?;
    sandbox::pin_lowest_cpu()?;
    let args = sandbox::pdf_args(&argv[1], &argv[2], page as u32, width as u32, height as u32);
    Err(PyErr::from(sandbox::execv(&args)))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(main, m)?)?;
    rs_common::export(m, &["main"])
}
