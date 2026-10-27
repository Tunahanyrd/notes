//! Rust port of: ink_render, ink_render_job, ink_render_worker, previews, page_image, thumbnails, pdf_background, pdf_background_worker, preview_codec, preview_contract, preview_worker. Oracle: the Python test suite (see samsung_notes/_rust.py).
// Without the `python` feature the PyO3 glue is compiled out; helpers and
// imports only that glue used are expected to be unused then.
#![cfg_attr(not(feature = "python"), allow(unused_imports, dead_code, unused_macros))]
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod pixels;
pub mod dataclasses;
pub mod raster;
pub mod pdf_layout;
pub mod sandbox;
pub mod pyhelp;
pub mod xarea;
pub mod ink_render;
pub mod ink_render_job;
pub mod ink_render_worker;
pub mod previews;
pub mod page_image;
pub mod thumbnails;
pub mod pdf_background;
pub mod pdf_background_worker;
pub mod preview_codec;
pub mod preview_contract;
pub mod preview_worker;

#[cfg(feature = "python")]
#[pymodule]
fn _render(m: &Bound<'_, PyModule>) -> PyResult<()> {
    rs_common::submodule(m, "ink_render", ink_render::register)?;
    rs_common::submodule(m, "ink_render_job", ink_render_job::register)?;
    rs_common::submodule(m, "ink_render_worker", ink_render_worker::register)?;
    rs_common::submodule(m, "previews", previews::register)?;
    rs_common::submodule(m, "page_image", page_image::register)?;
    rs_common::submodule(m, "thumbnails", thumbnails::register)?;
    rs_common::submodule(m, "pdf_background", pdf_background::register)?;
    rs_common::submodule(m, "pdf_background_worker", pdf_background_worker::register)?;
    rs_common::submodule(m, "preview_codec", preview_codec::register)?;
    rs_common::submodule(m, "preview_contract", preview_contract::register)?;
    rs_common::submodule(m, "preview_worker", preview_worker::register)?;
    Ok(())
}
