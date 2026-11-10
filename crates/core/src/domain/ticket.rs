use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Result, Severity};

/// System-assigned ticket identifier.
///
/// Backed by a UUIDv7, so ids sort by creation time, which keeps database
/// indexes append-friendly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TicketId(Uuid);

impl TicketId {
    /// Generates a fresh, time-ordered id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Returns the underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for TicketId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Uuid> for TicketId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl fmt::Display for TicketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for TicketId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Uuid::parse_str(s)
            .map(Self)
            .map_err(|e| Error::Validation(format!("invalid ticket id `{s}`: {e}")))
    }
}

/// Identifies the upstream client (customer or integration) a ticket belongs to.
///
/// Every ticket is scoped to a client so that one deployment can serve many
/// customers, each with its own tracker and routing rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ClientId(String);

impl ClientId {
    /// Maximum length of a client id.
    pub const MAX_LEN: usize = 128;

    /// Creates a client id. Allowed characters: ASCII alphanumerics, `-`, `_`, `.`.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let trimmed = value.trim();
        let valid_chars = trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if trimmed.is_empty() || trimmed.len() > Self::MAX_LEN || !valid_chars {
            return Err(Error::Validation(format!(
                "client id `{trimmed}` must be 1-{} characters of [A-Za-z0-9._-]",
                Self::MAX_LEN
            )));
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Returns the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ClientId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<ClientId> for String {
    fn from(value: ClientId) -> Self {
        value.0
    }
}

impl FromStr for ClientId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Points to the ticket in the external system it came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExternalRef {
    /// Connector instance name, such as `"jira-acme"`. Selects the
    /// [`Tracker`](crate::ports::Tracker) used to act on the ticket.
    pub system: String,
    /// Identifier in that system, such as `"OPS-1234"`.
    pub key: String,
}

impl ExternalRef {
    /// Creates a new external reference.
    pub fn new(system: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            key: key.into(),
        }
    }
}

impl fmt::Display for ExternalRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.system, self.key)
    }
}

/// Lifecycle of a ticket inside the handler.
///
/// ```text
/// Received -> Triaging -> AutoResolved
///                      -> Forwarded  -> Resolved
///                      -> Escalated  -> Resolved
///                      -> Failed     -> (retry) Triaging
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    /// Accepted and persisted, waiting for triage.
    Received,
    /// Claimed by a worker; triage is in progress.
    Triaging,
    /// Handled automatically using a known, automatable resolution.
    AutoResolved,
    /// Matched prior tickets; handed to intermediate support with a suggested fix.
    Forwarded,
    /// Novel or high-risk; escalated to a subject-matter expert.
    Escalated,
    /// Closed by a human. Its resolution has been fed back into the knowledge base.
    Resolved,
    /// Triage or dispatch failed; eligible for retry.
    Failed,
}

impl TicketStatus {
    /// Every status, in lifecycle order.
    pub const ALL: [Self; 7] = [
        Self::Received,
        Self::Triaging,
        Self::AutoResolved,
        Self::Forwarded,
        Self::Escalated,
        Self::Resolved,
        Self::Failed,
    ];

    /// Stable string form, used for storage and over the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Triaging => "triaging",
            Self::AutoResolved => "auto_resolved",
            Self::Forwarded => "forwarded",
            Self::Escalated => "escalated",
            Self::Resolved => "resolved",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for TicketStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TicketStatus {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == s)
            .ok_or_else(|| Error::Validation(format!("unknown ticket status `{s}`")))
    }
}

/// A ticket as delivered by an inbound adapter, before the system assigns an id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewTicket {
    /// Client the ticket belongs to.
    pub client: ClientId,
    /// Reference to the originating external ticket, if any.
    pub external_ref: Option<ExternalRef>,
    /// Short summary.
    pub title: String,
    /// Free-text description. This is the main signal for triage.
    pub description: String,
    /// Display name or email of the reporter.
    pub reporter: Option<String>,
    /// Free-form labels from the upstream system.
    pub labels: Vec<String>,
    /// Priority reported upstream. Treated as a hint, never trusted alone.
    pub reported_severity: Option<Severity>,
}

impl NewTicket {
    /// Maximum accepted title length, in bytes.
    pub const MAX_TITLE_LEN: usize = 512;
    /// Maximum accepted description length, in bytes.
    pub const MAX_DESCRIPTION_LEN: usize = 64 * 1024;

    /// Checks invariants that every inbound ticket must satisfy.
    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() {
            return Err(Error::Validation("title must not be empty".into()));
        }
        if self.title.len() > Self::MAX_TITLE_LEN {
            return Err(Error::Validation(format!(
                "title exceeds {} bytes",
                Self::MAX_TITLE_LEN
            )));
        }
        if self.description.len() > Self::MAX_DESCRIPTION_LEN {
            return Err(Error::Validation(format!(
                "description exceeds {} bytes",
                Self::MAX_DESCRIPTION_LEN
            )));
        }
        Ok(())
    }
}

/// A ticket owned by the handler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ticket {
    /// Internal id.
    pub id: TicketId,
    /// Client the ticket belongs to.
    pub client: ClientId,
    /// Reference to the originating external ticket, if any.
    pub external_ref: Option<ExternalRef>,
    /// Short summary.
    pub title: String,
    /// Free-text description.
    pub description: String,
    /// Display name or email of the reporter.
    pub reporter: Option<String>,
    /// Free-form labels.
    pub labels: Vec<String>,
    /// Priority reported upstream.
    pub reported_severity: Option<Severity>,
    /// Current lifecycle status.
    pub status: TicketStatus,
    /// When the ticket was received.
    pub created_at: DateTime<Utc>,
    /// When the ticket last changed.
    pub updated_at: DateTime<Utc>,
}

impl Ticket {
    /// Builds a freshly received ticket from inbound input, validating it first.
    pub fn receive(new: NewTicket, now: DateTime<Utc>) -> Result<Self> {
        new.validate()?;
        Ok(Self {
            id: TicketId::new(),
            client: new.client,
            external_ref: new.external_ref,
            title: new.title.trim().to_owned(),
            description: new.description,
            reporter: new.reporter,
            labels: new.labels,
            reported_severity: new.reported_severity,
            status: TicketStatus::Received,
            created_at: now,
            updated_at: now,
        })
    }

    /// The text used for embedding and similarity search.
    #[must_use]
    pub fn searchable_text(&self) -> String {
        searchable_text(&self.title, &self.description)
    }
}

/// Canonical text form used for embeddings.
///
/// Live tickets and knowledge-base entries must be embedded the same way,
/// otherwise their similarity scores are not comparable.
#[must_use]
pub fn searchable_text(title: &str, description: &str) -> String {
    format!("{}\n\n{}", title.trim(), description.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_ticket(title: &str) -> NewTicket {
        NewTicket {
            client: ClientId::new("acme").unwrap(),
            external_ref: None,
            title: title.into(),
            description: "Something broke".into(),
            reporter: None,
            labels: vec![],
            reported_severity: None,
        }
    }

    #[test]
    fn client_id_rejects_invalid_values() {
        assert!(ClientId::new("acme-corp_1.eu").is_ok());
        assert!(ClientId::new("  ").is_err());
        assert!(ClientId::new("acme corp").is_err());
        assert!(ClientId::new("a".repeat(129)).is_err());
    }

    #[test]
    fn client_id_deserialization_validates() {
        let ok: ClientId = serde_json::from_str("\"acme\"").unwrap();
        assert_eq!(ok.as_str(), "acme");
        assert!(serde_json::from_str::<ClientId>("\"bad id\"").is_err());
    }

    #[test]
    fn ticket_status_round_trips_through_strings() {
        for status in TicketStatus::ALL {
            assert_eq!(status.as_str().parse::<TicketStatus>().unwrap(), status);
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{}\"", status.as_str()));
        }
        assert!("bogus".parse::<TicketStatus>().is_err());
    }

    #[test]
    fn receive_validates_and_initialises_status() {
        let now = Utc::now();
        let ticket = Ticket::receive(new_ticket("  Login fails  "), now).unwrap();
        assert_eq!(ticket.status, TicketStatus::Received);
        assert_eq!(ticket.title, "Login fails");
        assert_eq!(ticket.created_at, now);

        assert!(Ticket::receive(new_ticket(""), now).is_err());
        assert!(Ticket::receive(new_ticket(&"x".repeat(513)), now).is_err());
    }

    #[test]
    fn ticket_ids_are_time_ordered_and_parseable() {
        let a = TicketId::new();
        let b = TicketId::new();
        assert!(a < b);
        assert_eq!(a.to_string().parse::<TicketId>().unwrap(), a);
        assert!("not-a-uuid".parse::<TicketId>().is_err());
    }
}
