//! Pure-Rust host glue for the Python-free engine (`crates/notes-engine`).
//!
//! * [`err`] — one error type ([`err::Fail`]) that keeps the Python exception
//!   class facts every `except (...)` clause of the oracle inspects.
//! * [`snapshot`] — `DocumentSnapshot.from_dict` / `to_dict` over
//!   `rs_store::val::Val`.
//! * [`deps`] — [`deps::PureDeps`], `rs_ops::deps::Deps` on the rs-doc /
//!   rs-ink cores (the `&dyn Any` contract is documented there).
//! * [`host`] — [`host::PureHost`], `rs_store::host::Host` for `StoreCore`.
//! * [`areas`] — editor/ink projections, page style/ops state, templates.
pub mod err;
pub mod snapshot;
pub mod deps;
pub mod host;
pub mod areas;
pub mod migration_projection;

pub use deps::PureDeps;
pub use err::{Fail, Kind};
pub use host::PureHost;
pub use rs_store::val::Val;
