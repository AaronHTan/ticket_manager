//! Plain data types describing tickets and the triage process.

mod category;
mod code;
mod embedding;
mod knowledge;
mod reporter;
mod severity;
mod ticket;
mod triage;

pub use category::Category;
pub use code::{
    BranchName, ChangeRequest, ChangeRequestRef, CodeContext, CommitId, FixOutcome, RepoId,
    RepoSpec, Signature, Workspace,
};
pub use embedding::{Embedding, top_k};
pub use knowledge::{HistoricalTicket, KnowledgeEntry, Resolution, SimilarTicket};
pub use reporter::{Reporter, TicketKind};
pub use severity::Severity;
pub use ticket::{
    ClientId, ExternalRef, Link, NewTicket, Ticket, TicketId, TicketStatus, searchable_text,
};
pub use triage::{
    Assignee, Classification, MatchSummary, RoutingDecision, SeverityAssessment, TriageReport,
};
