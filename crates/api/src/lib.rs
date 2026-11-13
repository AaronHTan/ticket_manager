//! The HTTP wire contract (v1), shared by the server and every Rust client.
//!
//! Keeping request and response types in their own crate means the server and
//! the SDK cannot drift apart, and a breaking change shows up as a compile
//! error in both. Domain enums ([`Severity`], [`TicketStatus`], ...) are reused
//! as they are because their serialised forms are already stable.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub use ticket_core::{
    Category, ClientId, CodeContext, ExternalRef, HistoricalTicket, KnowledgeEntry, Link,
    NewTicket, RepoId, Reporter, Severity, Ticket, TicketId, TicketKind, TicketStatus,
    TriageReport,
};

/// Route paths, relative to the server root.
pub mod paths {
    /// Liveness and readiness probe.
    pub const HEALTH: &str = "/healthz";
    /// Submit (`POST`) tickets.
    pub const TICKETS: &str = "/v1/tickets";
    /// Seed the knowledge base with resolved history.
    pub const KNOWLEDGE_IMPORT: &str = "/v1/knowledge/import";

    /// `GET` a single ticket.
    #[must_use]
    pub fn ticket(id: super::TicketId) -> String {
        format!("{TICKETS}/{id}")
    }

    /// `POST` a human resolution for a ticket.
    #[must_use]
    pub fn ticket_resolution(id: super::TicketId) -> String {
        format!("{TICKETS}/{id}/resolution")
    }

    /// Jira webhook endpoint for connector `name`.
    #[must_use]
    pub fn jira_webhook(name: &str) -> String {
        format!("/v1/webhooks/jira/{name}")
    }
}

/// Body of `POST /v1/tickets`.
///
/// Only `client` and `title` are required. `reporter` accepts either a plain
/// string (name or email) or an object `{ "name", "email", "external_id" }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitTicketRequest {
    /// Client (tenant) submitting the ticket.
    pub client: ClientId,
    /// Short summary.
    pub title: String,
    /// Full description.
    #[serde(default)]
    pub description: String,
    /// Who raised the ticket.
    #[serde(default)]
    pub reporter: Option<Reporter>,
    /// What sort of request this is.
    #[serde(default)]
    pub kind: TicketKind,
    /// Free-form labels.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Components or modules affected.
    #[serde(default)]
    pub components: Vec<String>,
    /// Reporter's severity estimate.
    #[serde(default)]
    pub severity: Option<Severity>,
    /// Hints about the code involved.
    #[serde(default)]
    pub code_context: Option<CodeContext>,
    /// Attachments and related links.
    #[serde(default)]
    pub links: Vec<Link>,
    /// When the ticket was raised in the caller's system.
    #[serde(default)]
    pub reported_at: Option<DateTime<Utc>>,
    /// Free-form extras.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    /// Reference to the ticket in the caller's own system, which makes
    /// submissions idempotent.
    #[serde(default)]
    pub external_ref: Option<ExternalRef>,
}

impl SubmitTicketRequest {
    /// A request with the required fields set and everything else empty.
    pub fn new(client: ClientId, title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            client,
            title: title.into(),
            description: description.into(),
            reporter: None,
            kind: TicketKind::default(),
            labels: Vec::new(),
            components: Vec::new(),
            severity: None,
            code_context: None,
            links: Vec::new(),
            reported_at: None,
            metadata: BTreeMap::new(),
            external_ref: None,
        }
    }
}

impl From<SubmitTicketRequest> for NewTicket {
    fn from(value: SubmitTicketRequest) -> Self {
        Self {
            client: value.client,
            external_ref: value.external_ref,
            title: value.title,
            description: value.description,
            reporter: value.reporter,
            kind: value.kind,
            labels: value.labels,
            components: value.components,
            reported_severity: value.severity,
            code_context: value.code_context,
            links: value.links,
            reported_at: value.reported_at,
            metadata: value.metadata,
        }
    }
}

/// Response to `POST /v1/tickets` (`202 Accepted`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitTicketResponse {
    /// Assigned id.
    pub id: TicketId,
    /// Status at the time of the response.
    pub status: TicketStatus,
    /// `true` if an existing ticket with the same external reference was returned.
    pub duplicate: bool,
}

/// Response to `GET /v1/tickets/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketView {
    /// The ticket.
    #[serde(flatten)]
    pub ticket: Ticket,
    /// Latest triage report, once triage has run.
    pub triage: Option<TriageReport>,
}

/// Body of `POST /v1/tickets/{id}/resolution`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordResolutionRequest {
    /// What fixed the issue.
    pub summary: String,
    /// Whether this fix may be applied automatically to future matches.
    #[serde(default)]
    pub automatable: bool,
    /// Who resolved it.
    #[serde(default)]
    pub resolved_by: Option<String>,
    /// Corrected category.
    #[serde(default)]
    pub category: Option<Category>,
    /// Corrected severity.
    #[serde(default)]
    pub severity: Option<Severity>,
}

/// Body of `POST /v1/knowledge/import`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportHistoryRequest {
    /// Resolved tickets to index.
    pub tickets: Vec<HistoricalTicket>,
}

/// Response to `POST /v1/knowledge/import`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportHistoryResponse {
    /// Number of entries indexed.
    pub imported: usize,
}

/// Response to `GET /healthz`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    /// `"ok"` when healthy.
    pub status: String,
    /// Server version.
    pub version: String,
    /// Knowledge-base size.
    pub knowledge_entries: u64,
}

/// Body of every non-2xx response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Machine-readable error code, such as `not_found`.
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_request_accepts_minimal_body_and_rejects_unknown_fields() {
        let req: SubmitTicketRequest =
            serde_json::from_str(r#"{"client":"acme","title":"Login fails"}"#).unwrap();
        assert_eq!(req.description, "");
        assert_eq!(req.kind, TicketKind::Other);
        assert!(
            serde_json::from_str::<SubmitTicketRequest>(
                r#"{"client":"acme","title":"x","priority":"p1"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn submit_request_accepts_full_metadata() {
        let req: SubmitTicketRequest = serde_json::from_str(
            r#"{
                "client": "acme",
                "title": "Checkout 500",
                "reporter": {"name": "Jane Doe", "email": "jane@acme.test"},
                "kind": "bug",
                "components": ["checkout"],
                "code_context": {"repository": "web-app", "paths": ["src/cart.rs"]},
                "links": [{"url": "https://logs.acme.test/1", "title": "logs"}],
                "reported_at": "2026-11-01T09:30:00Z",
                "metadata": {"browser": "firefox"}
            }"#,
        )
        .unwrap();
        let new = NewTicket::from(req);
        assert!(new.validate().is_ok());
        assert_eq!(new.kind, TicketKind::Bug);
        assert_eq!(new.reporter.unwrap().name.as_deref(), Some("Jane Doe"));
        assert_eq!(
            new.code_context.unwrap().repository.unwrap().as_str(),
            "web-app"
        );
        assert_eq!(new.metadata["browser"], "firefox");

        let legacy: SubmitTicketRequest =
            serde_json::from_str(r#"{"client":"acme","title":"x","reporter":"jane@acme.test"}"#)
                .unwrap();
        assert_eq!(
            legacy.reporter.unwrap().email.as_deref(),
            Some("jane@acme.test")
        );
    }

    #[test]
    fn paths_are_built_from_ids() {
        let id = TicketId::new();
        assert_eq!(paths::ticket(id), format!("/v1/tickets/{id}"));
        assert_eq!(paths::jira_webhook("acme"), "/v1/webhooks/jira/acme");
    }
}
