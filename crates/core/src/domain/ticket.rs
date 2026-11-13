use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::code::check_len;
use crate::{CodeContext, Error, RepoId, Reporter, Result, Severity, TicketKind};

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

/// A link attached to a ticket: a screenshot, log file, stack trace or related page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Absolute `http` or `https` URL.
    pub url: String,
    /// What the link points to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Link {
    /// Maximum URL length, in bytes.
    pub const MAX_URL_LEN: usize = 2048;
    /// Maximum title length, in bytes.
    pub const MAX_TITLE_LEN: usize = 256;

    /// Creates a link without a title.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            title: None,
        }
    }

    /// Accepts only absolute `http(s)` URLs without whitespace.
    pub fn validate(&self) -> Result<()> {
        check_len("link url", &self.url, Self::MAX_URL_LEN)?;
        let rest = self
            .url
            .strip_prefix("https://")
            .or_else(|| self.url.strip_prefix("http://"));
        if rest.is_none_or(|r| r.is_empty())
            || self
                .url
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(Error::Validation(format!(
                "link `{}` must be an absolute http(s) URL",
                self.url
            )));
        }
        if let Some(title) = &self.title {
            check_len("link title", title, Self::MAX_TITLE_LEN)?;
        }
        Ok(())
    }
}

/// A ticket as delivered by an inbound adapter, before the system assigns an id.
///
/// Build one with [`NewTicket::new`] and the `with_*` methods so that adding
/// fields later does not break callers:
///
/// ```
/// # use ticket_core::{ClientId, NewTicket, TicketKind};
/// let ticket = NewTicket::new(ClientId::new("acme").unwrap(), "Login fails", "500 on submit")
///     .with_kind(TicketKind::Bug)
///     .with_components(["auth"]);
/// assert!(ticket.validate().is_ok());
/// ```
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
    /// Who raised the ticket.
    pub reporter: Option<Reporter>,
    /// What sort of request this is.
    #[serde(default)]
    pub kind: TicketKind,
    /// Free-form labels from the upstream system.
    pub labels: Vec<String>,
    /// Components or modules named upstream. Used to find the right repository.
    #[serde(default)]
    pub components: Vec<String>,
    /// Priority reported upstream. Treated as a hint, never trusted alone.
    pub reported_severity: Option<Severity>,
    /// Hints about the code involved.
    #[serde(default)]
    pub code_context: Option<CodeContext>,
    /// Attachments and related links.
    #[serde(default)]
    pub links: Vec<Link>,
    /// When the ticket was created upstream.
    #[serde(default)]
    pub reported_at: Option<DateTime<Utc>>,
    /// Connector-specific extras that have no field of their own.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl NewTicket {
    /// Maximum accepted title length, in bytes.
    pub const MAX_TITLE_LEN: usize = 512;
    /// Maximum accepted description length, in bytes.
    pub const MAX_DESCRIPTION_LEN: usize = 64 * 1024;
    /// Maximum number of labels or components.
    pub const MAX_TAGS: usize = 64;
    /// Maximum length of one label or component, in bytes.
    pub const MAX_TAG_LEN: usize = 128;
    /// Maximum number of links.
    pub const MAX_LINKS: usize = 32;
    /// Maximum number of metadata entries.
    pub const MAX_METADATA_ENTRIES: usize = 32;
    /// Maximum metadata key length, in bytes.
    pub const MAX_METADATA_KEY_LEN: usize = 64;
    /// Maximum metadata value length, in bytes.
    pub const MAX_METADATA_VALUE_LEN: usize = 1024;

    /// Creates a ticket with the required fields. Everything else is empty.
    pub fn new(client: ClientId, title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            client,
            external_ref: None,
            title: title.into(),
            description: description.into(),
            reporter: None,
            kind: TicketKind::default(),
            labels: Vec::new(),
            components: Vec::new(),
            reported_severity: None,
            code_context: None,
            links: Vec::new(),
            reported_at: None,
            metadata: BTreeMap::new(),
        }
    }

    /// Sets the external reference.
    #[must_use]
    pub fn with_external_ref(mut self, external_ref: ExternalRef) -> Self {
        self.external_ref = Some(external_ref);
        self
    }

    /// Sets the reporter.
    #[must_use]
    pub fn with_reporter(mut self, reporter: Reporter) -> Self {
        self.reporter = Some(reporter);
        self
    }

    /// Sets the kind.
    #[must_use]
    pub fn with_kind(mut self, kind: TicketKind) -> Self {
        self.kind = kind;
        self
    }

    /// Sets the labels.
    #[must_use]
    pub fn with_labels<S: Into<String>>(mut self, labels: impl IntoIterator<Item = S>) -> Self {
        self.labels = labels.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the components.
    #[must_use]
    pub fn with_components<S: Into<String>>(
        mut self,
        components: impl IntoIterator<Item = S>,
    ) -> Self {
        self.components = components.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the reported severity.
    #[must_use]
    pub fn with_reported_severity(mut self, severity: Severity) -> Self {
        self.reported_severity = Some(severity);
        self
    }

    /// Sets the code context.
    #[must_use]
    pub fn with_code_context(mut self, code_context: CodeContext) -> Self {
        self.code_context = Some(code_context);
        self
    }

    /// Points the ticket at a repository, keeping any other code hints.
    #[must_use]
    pub fn with_repository(mut self, repository: RepoId) -> Self {
        self.code_context.get_or_insert_default().repository = Some(repository);
        self
    }

    /// Adds a link.
    #[must_use]
    pub fn with_link(mut self, link: Link) -> Self {
        self.links.push(link);
        self
    }

    /// Checks invariants that every inbound ticket must satisfy.
    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() {
            return Err(Error::Validation("title must not be empty".into()));
        }
        check_len("title", &self.title, Self::MAX_TITLE_LEN)?;
        check_len("description", &self.description, Self::MAX_DESCRIPTION_LEN)?;
        if let Some(reporter) = &self.reporter {
            reporter.validate()?;
        }
        validate_tags("labels", &self.labels)?;
        validate_tags("components", &self.components)?;
        if let Some(code_context) = &self.code_context {
            code_context.validate()?;
        }
        if self.links.len() > Self::MAX_LINKS {
            return Err(Error::Validation(format!(
                "at most {} links are allowed",
                Self::MAX_LINKS
            )));
        }
        for link in &self.links {
            link.validate()?;
        }
        if self.metadata.len() > Self::MAX_METADATA_ENTRIES {
            return Err(Error::Validation(format!(
                "at most {} metadata entries are allowed",
                Self::MAX_METADATA_ENTRIES
            )));
        }
        for (key, value) in &self.metadata {
            if key.trim().is_empty() {
                return Err(Error::Validation("metadata keys must not be empty".into()));
            }
            check_len("metadata key", key, Self::MAX_METADATA_KEY_LEN)?;
            check_len("metadata value", value, Self::MAX_METADATA_VALUE_LEN)?;
        }
        Ok(())
    }
}

fn validate_tags(what: &str, tags: &[String]) -> Result<()> {
    if tags.len() > NewTicket::MAX_TAGS {
        return Err(Error::Validation(format!(
            "at most {} {what} are allowed",
            NewTicket::MAX_TAGS
        )));
    }
    for tag in tags {
        if tag.trim().is_empty() {
            return Err(Error::Validation(format!("{what} must not be empty")));
        }
        check_len(what, tag, NewTicket::MAX_TAG_LEN)?;
    }
    Ok(())
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
    /// Who raised the ticket.
    pub reporter: Option<Reporter>,
    /// What sort of request this is.
    #[serde(default)]
    pub kind: TicketKind,
    /// Free-form labels.
    pub labels: Vec<String>,
    /// Components or modules named upstream.
    #[serde(default)]
    pub components: Vec<String>,
    /// Priority reported upstream.
    pub reported_severity: Option<Severity>,
    /// Hints about the code involved.
    #[serde(default)]
    pub code_context: Option<CodeContext>,
    /// Attachments and related links.
    #[serde(default)]
    pub links: Vec<Link>,
    /// When the ticket was created upstream.
    #[serde(default)]
    pub reported_at: Option<DateTime<Utc>>,
    /// Connector-specific extras.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
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
            kind: new.kind,
            labels: new.labels,
            components: new.components,
            reported_severity: new.reported_severity,
            code_context: new.code_context.filter(|c| !c.is_empty()),
            links: new.links,
            reported_at: new.reported_at,
            metadata: new.metadata,
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
        NewTicket::new(ClientId::new("acme").unwrap(), title, "Something broke")
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
        assert_eq!(ticket.kind, TicketKind::Other);

        assert!(Ticket::receive(new_ticket(""), now).is_err());
        assert!(Ticket::receive(new_ticket(&"x".repeat(513)), now).is_err());
    }

    #[test]
    fn receive_carries_metadata_and_drops_empty_code_context() {
        let reporter = Reporter::from_label("jane@acme.test").unwrap();
        let new = new_ticket("Checkout 500")
            .with_reporter(reporter.clone())
            .with_kind(TicketKind::Bug)
            .with_components(["checkout"])
            .with_repository(RepoId::new("web-app").unwrap())
            .with_link(Link::new("https://logs.acme.test/run/42"));
        let ticket = Ticket::receive(new, Utc::now()).unwrap();
        assert_eq!(ticket.reporter, Some(reporter));
        assert_eq!(ticket.kind, TicketKind::Bug);
        assert_eq!(ticket.components, ["checkout"]);
        assert_eq!(
            ticket.code_context.unwrap().repository.unwrap().as_str(),
            "web-app"
        );
        assert_eq!(ticket.links.len(), 1);

        let empty = new_ticket("x").with_code_context(CodeContext::default());
        assert_eq!(
            Ticket::receive(empty, Utc::now()).unwrap().code_context,
            None
        );
    }

    #[test]
    fn validate_rejects_bad_metadata() {
        let bad_link = new_ticket("x").with_link(Link::new("javascript:alert(1)"));
        assert!(bad_link.validate().is_err());
        assert!(
            new_ticket("x")
                .with_link(Link::new("https://"))
                .validate()
                .is_err()
        );
        assert!(
            new_ticket("x")
                .with_link(Link::new("https://a.test/x y"))
                .validate()
                .is_err()
        );

        assert!(new_ticket("x").with_components([""]).validate().is_err());
        let many = vec!["c"; NewTicket::MAX_TAGS + 1];
        assert!(new_ticket("x").with_components(many).validate().is_err());
        assert!(
            new_ticket("x")
                .with_labels(["l".repeat(129)])
                .validate()
                .is_err()
        );

        let mut meta = new_ticket("x");
        meta.metadata.insert("k".repeat(65), "v".into());
        assert!(meta.validate().is_err());
        let mut meta = new_ticket("x");
        meta.metadata.insert(" ".into(), "v".into());
        assert!(meta.validate().is_err());

        let traversal = new_ticket("x").with_code_context(CodeContext {
            paths: vec!["../../etc/passwd".into()],
            ..CodeContext::default()
        });
        assert!(traversal.validate().is_err());
    }

    #[test]
    fn new_ticket_deserialises_without_new_fields() {
        let json = r#"{
            "client": "acme", "external_ref": null, "title": "t", "description": "d",
            "reporter": "jane@acme.test", "labels": [], "reported_severity": null
        }"#;
        let ticket: NewTicket = serde_json::from_str(json).unwrap();
        assert_eq!(ticket.kind, TicketKind::Other);
        assert_eq!(
            ticket.reporter.unwrap().email.as_deref(),
            Some("jane@acme.test")
        );
        assert!(ticket.links.is_empty());
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
