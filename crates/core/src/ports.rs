//! Ports: the capabilities the domain needs from the outside world.
//!
//! Each trait is implemented by one or more adapter crates:
//!
//! | Port                 | Adapters                                         |
//! |----------------------|--------------------------------------------------|
//! | [`TicketRepository`] | `ticket-storage` (SQLite, in-memory)             |
//! | [`KnowledgeBase`]    | `ticket-storage` (SQLite, in-memory)             |
//! | [`Embedder`]         | `ticket-retrieval` (hashing; neural models later) |
//! | [`Classifier`]       | `ticket-engine` (rule-based; LLM later)          |
//! | [`Tracker`]          | `ticket-jira` (more connectors later)            |
//! | [`Notifier`]         | `ticket-engine` (log); Slack/email later         |
//! | [`SourceControl`]    | `ticket-git` (git CLI)                           |
//! | [`CodeHost`]         | `ticket-engine` (log); GitHub/GitLab later       |
//! | [`Fixer`]            | `ticket-engine` (no-op); LLM agent later         |
//!
//! Traits use `async_trait` so they can be stored as `Arc<dyn Port>` and
//! picked from configuration at runtime. That is what lets one binary serve
//! several clients with different backends.

use async_trait::async_trait;

use crate::{
    Assignee, BranchName, ChangeRequest, ChangeRequestRef, Classification, CommitId, Embedding,
    ExternalRef, FixOutcome, KnowledgeEntry, RepoSpec, Result, RoutingDecision, Severity,
    Signature, SimilarTicket, Ticket, TicketId, TicketStatus, TriageReport, Workspace,
};

/// Persistence for live tickets and their triage history.
#[async_trait]
pub trait TicketRepository: Send + Sync {
    /// Inserts a new ticket. Fails with [`Error::Conflict`](crate::Error::Conflict)
    /// if the id or external reference already exists.
    async fn insert(&self, ticket: &Ticket) -> Result<()>;

    /// Fetches a ticket by id.
    async fn get(&self, id: TicketId) -> Result<Option<Ticket>>;

    /// Fetches a ticket by its external reference.
    async fn find_by_external_ref(&self, external: &ExternalRef) -> Result<Option<Ticket>>;

    /// Unconditionally sets a ticket's status.
    async fn set_status(&self, id: TicketId, status: TicketStatus) -> Result<()>;

    /// Atomically moves a ticket to `to` if its current status is one of `from`.
    /// Returns `true` if the transition happened. Used by workers to claim tickets.
    async fn transition(
        &self,
        id: TicketId,
        from: &[TicketStatus],
        to: TicketStatus,
    ) -> Result<bool>;

    /// Lists tickets with a given status, oldest first.
    async fn list_by_status(&self, status: TicketStatus, limit: u32) -> Result<Vec<Ticket>>;

    /// Appends a triage report.
    async fn save_triage(&self, report: &TriageReport) -> Result<()>;

    /// Returns the most recent triage report for a ticket.
    async fn latest_triage(&self, id: TicketId) -> Result<Option<TriageReport>>;
}

/// The retrieval store of resolved tickets: the "R" in RAG.
#[async_trait]
pub trait KnowledgeBase: Send + Sync {
    /// Inserts or replaces an entry with its embedding.
    async fn upsert(&self, entry: &KnowledgeEntry, embedding: &Embedding) -> Result<()>;

    /// Returns up to `limit` entries most similar to `query`, best first.
    async fn search(&self, query: &Embedding, limit: usize) -> Result<Vec<SimilarTicket>>;

    /// Number of entries stored.
    async fn count(&self) -> Result<u64>;
}

/// Turns text into vectors for similarity search.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Identifies the model and its version. Vectors from different models
    /// are not comparable, so a change here means re-indexing.
    fn model_id(&self) -> &str;

    /// Embeds a piece of text.
    async fn embed(&self, text: &str) -> Result<Embedding>;
}

/// Assigns severity and category to a ticket.
#[async_trait]
pub trait Classifier: Send + Sync {
    /// Classifies `ticket`, using `matches` (similar resolved tickets) as evidence.
    async fn classify(&self, ticket: &Ticket, matches: &[SimilarTicket]) -> Result<Classification>;
}

/// The outbound side of an external ticketing system (Jira, ServiceNow, Zendesk, ...).
#[async_trait]
pub trait Tracker: Send + Sync {
    /// Connector instance name. Matches [`ExternalRef::system`].
    fn system(&self) -> &str;

    /// Adds a comment to the external ticket.
    async fn comment(&self, external: &ExternalRef, body: &str) -> Result<()>;

    /// Assigns the external ticket.
    async fn assign(&self, external: &ExternalRef, assignee: &Assignee) -> Result<()>;

    /// Updates the external ticket's priority to reflect the assessed severity.
    async fn set_severity(&self, external: &ExternalRef, severity: Severity) -> Result<()>;

    /// Marks the external ticket resolved.
    async fn mark_resolved(&self, external: &ExternalRef, resolution: &str) -> Result<()>;
}

/// An out-of-band notification channel, such as email, Slack or a pager.
#[async_trait]
pub trait Notifier: Send + Sync {
    /// Tells `assignee` that `ticket` was routed to them.
    async fn notify(
        &self,
        assignee: &Assignee,
        ticket: &Ticket,
        decision: &RoutingDecision,
    ) -> Result<()>;
}

/// Checking out, changing and publishing code in a git repository.
///
/// Every ticket works in its own [`Workspace`], so several tickets can change
/// the same repository at once.
#[async_trait]
pub trait SourceControl: Send + Sync {
    /// Brings the local copy of `repo` up to date and creates a private
    /// working tree for `ticket` on a new `branch` from the repository's
    /// default branch. Fails with [`Error::Conflict`](crate::Error::Conflict)
    /// if a workspace or branch for it already exists.
    async fn prepare(
        &self,
        repo: &RepoSpec,
        ticket: TicketId,
        branch: &BranchName,
    ) -> Result<Workspace>;

    /// The uncommitted and committed changes since the base commit,
    /// including new files, as a unified diff.
    async fn diff(&self, workspace: &Workspace) -> Result<String>;

    /// Commits every change in the working tree. Returns `None` if there was
    /// nothing to commit.
    async fn commit(
        &self,
        workspace: &Workspace,
        message: &str,
        author: &Signature,
    ) -> Result<Option<CommitId>>;

    /// Publishes the workspace branch to the remote.
    async fn push(&self, workspace: &Workspace) -> Result<()>;

    /// Deletes the working tree and its local branch. Pushed work is kept on the remote.
    async fn release(&self, workspace: &Workspace) -> Result<()>;
}

/// A code-review host (GitHub, GitLab, Bitbucket, ...).
#[async_trait]
pub trait CodeHost: Send + Sync {
    /// Opens a pull or merge request for an already pushed branch.
    async fn open_change_request(&self, request: &ChangeRequest) -> Result<ChangeRequestRef>;
}

/// Something that tries to fix a ticket by editing files in a workspace.
#[async_trait]
pub trait Fixer: Send + Sync {
    /// Edits `workspace` to address `ticket`. Leaving the tree unchanged is a
    /// valid outcome: it means no fix was found.
    async fn attempt(&self, ticket: &Ticket, workspace: &Workspace) -> Result<FixOutcome>;
}
