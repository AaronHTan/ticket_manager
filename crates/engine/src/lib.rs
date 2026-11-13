//! The triage engine: application logic built only on `ticket-core` ports.
//!
//! ```text
//! NewTicket ─ingest─▶ Ticket(received) ─queue─▶ worker ─process─▶
//!     embed ▶ search knowledge base ▶ classify ▶ route ▶ dispatch ▶ status
//! ```
//!
//! * [`classifier`]: severity and category from keywords, reporter hints and
//!   similar past tickets.
//! * [`routing`]: a pure function from (classification, matches) to a
//!   [`RoutingDecision`](ticket_core::RoutingDecision).
//! * [`dispatch`]: carries out a decision against external trackers and notifiers.
//! * [`pipeline`]: orchestrates the steps and the learning feedback loop.
//! * [`worker`]: a bounded queue and worker pool for asynchronous processing.
//! * [`fix`]: resolving a ticket to a repository and proposing a code change
//!   from a throwaway workspace (not yet wired into triage).

pub mod classifier;
pub mod dispatch;
pub mod fix;
pub mod pipeline;
pub mod routing;
pub mod worker;

pub use classifier::{KeywordRule, RuleBasedClassifier};
pub use dispatch::{Dispatcher, LogNotifier};
pub use fix::{
    FixCoordinator, FixResult, LogCodeHost, NoopFixer, RepositoryConfig, RepositoryDirectory,
};
pub use pipeline::{Ingested, PipelineDeps, TriagePipeline};
pub use routing::{EscalationDirectory, RoutingPolicy, TicketRouter};
pub use worker::{TriageQueue, WorkerPool};
