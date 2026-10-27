//! Replacement for the Python module `preview_codec` (historical surface).
//!
//! The emulator-backed provider that ran the user's native SPI codec is
//! retired; its original glue is kept in reference/spi-emulator-glue/. The
//! product decoder is notes-preview's pure-Rust `spi` module.
use crate::pixels as px;
use crate::xarea;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyTuple};

#[cfg(feature = "python")]
#[pyclass(extends=pyo3::exceptions::PyValueError, subclass, module="samsung_notes.preview_codec", name="PreviewCodecError")]
pub struct PreviewCodecError {
    #[pyo3(get, set)]
    code: Py<PyAny>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PreviewCodecError {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(py: Python<'_>, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        PreviewCodecError { code: py.None() }
    }

    #[cfg(feature = "python")]
    fn __init__(slf: &Bound<'_, Self>, code: Bound<'_, PyAny>, message: Bound<'_, PyAny>) -> PyResult<()> {
        slf.setattr("args", (message,))?;
        slf.borrow_mut().code = code.unbind();
        Ok(())
    }
}

#[cfg(feature = "python")]
fn codec_error(py: Python<'_>, code: &str, message: &str) -> PyErr {
    match py.get_type::<PreviewCodecError>().call1((code, message)) {
        Ok(v) => PyErr::from_value(v),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn provider_available() -> bool {
    // The native-library emulator provider is retired (2026-10-27); the
    // product decodes canvas caches with notes-preview's pure-Rust `spi`
    // module. This historical Python surface reports the provider absent.
    false
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature=(raw, *, expected_size=None))]
pub fn decode_spi<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>, expected_size: Option<(i64, i64)>) -> PyResult<Bound<'py, PyAny>> {
    let header = match xarea::inspect_spi(py, raw) {
        Ok(h) => h,
        Err(e) if e.is_instance(py, &xarea::class(py, "samsung_notes.ink", "InkError")?) => {
            return Err(codec_error(py, "unsupported", "Bu çizim önbelleği biçimi desteklenmiyor."))
        }
        Err(e) => return Err(e),
    };
    let (width, height) = (header.width as i64, header.height as i64);
    if width * height > px::MAX_PIXELS {
        return Err(codec_error(py, "too_large", "Bu sayfa önizleme boyut sınırını aşıyor."));
    }
    let (flags, color, capacity) = (header.flags as i64, header.color_index as i64, header.reference_cache_capacity as i64);
    if flags != 0xe0 || color != 4 || capacity != 0 {
        return Err(codec_error(py, "unsupported", "Bu çizim önbelleği çeşidi henüz desteklenmiyor."));
    }
    if expected_size.is_some_and(|s| s != (width, height)) {
        return Err(codec_error(py, "size_mismatch", "Sayfa önbelleğinin boyut bilgisi uyuşmuyor."));
    }
    Err(codec_error(py, "unavailable", "Yerel çizim önizleme bileşeni kurulu değil."))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PreviewCodecError>()?;
    m.add_function(wrap_pyfunction!(provider_available, m)?)?;
    m.add_function(wrap_pyfunction!(decode_spi, m)?)?;
    rs_common::export(m, &["PreviewCodecError", "provider_available", "decode_spi"])
}
