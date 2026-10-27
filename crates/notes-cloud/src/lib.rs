//! Pure-Rust port of the Cloud transaction layer (no PyO3, no Python):
//!
//! * [`api`] + [`writes`] — `samsung_notes/cloud_api.py` (`CloudAPI` →
//!   [`CloudApi`]);
//! * [`metadata`] — `cloud_metadata.py` (`CloudMetadata`);
//! * [`ops`] — `cloud_ink_erase.py` and `cloud_page_ops.py`;
//! * [`transport`] / [`auth`] — the network and credential-renewal seams
//!   ([`Transport`], [`Renewal`]) the auth port plugs into.
//!
//! Results are [`Val`] dicts with the keys and values the Python methods
//! return; failures are [`Fail`] values carrying the Python exception class,
//! `.code`, Turkish `.message` and `.ambiguous`. On-disk receipts are
//! byte-identical to the oracle's.
pub mod api;
pub mod auth;
pub mod editor;
pub mod etree;
pub mod guards;
pub mod json;
pub mod metadata;
pub mod ops;
pub mod transport;
pub mod util;
pub mod writes;

pub use api::{CloudApi, Options, SnapIn};
pub use auth::{PortedRenewal, Renewal, StubRenewal};
pub use metadata::CloudMetadata;
pub use notes_host::snapshot::Snapshot;
pub use notes_host::{Fail, Kind, Val};
pub use transport::{Headers, NoNetwork, Request, Response, Transport};
pub use util::{OsRandom, Random};
pub use writes::{samples_from_points, InkRecipe};
