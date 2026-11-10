//! Persistence adapters for [`TicketRepository`] and [`KnowledgeBase`].
//!
//! * [`InMemoryStore`]: no setup; for tests, demos and ephemeral runs.
//! * [`SqliteStore`] (feature `sqlite`, on by default): a single-file database
//!   with WAL journaling and embedded migrations.
//!
//! Both pass the same contract test suite (`tests/contract.rs`). A Postgres
//! adapter (with `pgvector` for similarity search) would be added as another
//! module behind its own feature and run against that same suite.
//!
//! [`TicketRepository`]: ticket_core::ports::TicketRepository
//! [`KnowledgeBase`]: ticket_core::ports::KnowledgeBase

mod memory;
#[cfg(feature = "sqlite")]
mod sqlite;

pub use memory::InMemoryStore;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStore;
