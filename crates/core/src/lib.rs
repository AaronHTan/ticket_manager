//! Domain model and ports shared by every ticket-handler crate.
//!
//! `ticket-core` is the innermost layer of a hexagonal (ports & adapters)
//! architecture. It defines *what* the system reasons about -- tickets,
//! severities, knowledge-base entries, routing decisions -- and *which*
//! capabilities it needs from the outside world (persistence, retrieval,
//! external trackers, notifications) as traits in [`ports`].
//!
//! It performs no I/O and has no opinion about databases, HTTP frameworks or
//! vendors, so every other crate can depend on it without pulling those in.

pub mod domain;
pub mod error;
pub mod ports;

pub use domain::*;
pub use error::{BoxError, Error, Result};
