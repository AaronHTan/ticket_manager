//! Plain data types describing tickets and the triage process.

mod category;
mod embedding;
mod knowledge;
mod severity;
mod ticket;
mod triage;

pub use category::Category;
pub use embedding::{Embedding, top_k};
pub use knowledge::{HistoricalTicket, KnowledgeEntry, Resolution, SimilarTicket};
pub use severity::Severity;
pub use ticket::{
    ClientId, ExternalRef, NewTicket, Ticket, TicketId, TicketStatus, searchable_text,
};
pub use triage::{
    Assignee, Classification, MatchSummary, RoutingDecision, SeverityAssessment, TriageReport,
};
