//! The ticket-handler HTTP service, as a library so integration tests (and
//! alternative binaries) can build the exact same app.

pub mod bootstrap;
pub mod config;
mod error;
mod routes;
pub mod seed;
mod state;

pub use bootstrap::{App, build};
pub use config::AppConfig;
pub use routes::router;
pub use state::AppState;
