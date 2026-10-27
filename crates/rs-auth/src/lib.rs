//! Rust port of the `auth` area (login/session/network path). Oracle: the
//! Python test suite (see samsung_notes/_rust.py). This area is ported last and
//! deliberately keeps the real network / subprocess / desktop-handler paths in
//! Python; see crates/rs-auth/PORTING.md for the exact split and what needs a
//! live re-validation (sign-in, token renewal).
// Without the `python` feature the PyO3 glue is compiled out; helpers and
// imports only that glue used are expected to be unused then.
#![cfg_attr(not(feature = "python"), allow(unused_imports, dead_code, unused_macros))]
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod common;

pub mod await_notes_login;
pub mod capture_oauth_callback;
pub mod exchange_account_code;
pub mod notes_auth_bridge;
pub mod notes_cloud;
pub mod notes_login;
pub mod probe_account_entrypoint;
pub mod session;

#[cfg(feature = "python")]
#[pymodule]
fn _auth(m: &Bound<'_, PyModule>) -> PyResult<()> {
    rs_common::submodule(m, "notes_login", notes_login::register)?;
    rs_common::submodule(m, "exchange_account_code", exchange_account_code::register)?;
    rs_common::submodule(m, "capture_oauth_callback", capture_oauth_callback::register)?;
    rs_common::submodule(m, "notes_auth_bridge", notes_auth_bridge::register)?;
    rs_common::submodule(m, "notes_cloud", notes_cloud::register_module)?;
    rs_common::submodule(m, "probe_account_entrypoint", probe_account_entrypoint::register)?;
    rs_common::submodule(m, "await_notes_login", await_notes_login::register)?;
    rs_common::submodule(m, "session", session::register)?;
    Ok(())
}
