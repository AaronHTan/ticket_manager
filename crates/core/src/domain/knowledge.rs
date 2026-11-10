use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Category, Error, Result, Severity, TicketId, searchable_text};

/// How a ticket was resolved. This is the unit of knowledge the system learns from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    /// Human-readable description of the fix. Posted to the reporter on auto-resolution.
    pub summary: String,
    /// Whether a human has confirmed this fix can be applied without review.
    /// Only automatable resolutions are ever used to auto-resolve.
    #[serde(default)]
    pub automatable: bool,
    /// Who resolved the original ticket.
    #[serde(default)]
    pub resolved_by: Option<String>,
}

/// A resolved ticket stored in the knowledge base.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeEntry {
    /// Id of the ticket this knowledge came from. Imported history gets a fresh id.
    pub ticket_id: TicketId,
    /// Ticket title.
    pub title: String,
    /// Ticket description.
    pub description: String,
    /// Problem category, if known.
    pub category: Option<Category>,
    /// Final, human-confirmed severity.
    pub severity: Severity,
    /// How it was fixed.
    pub resolution: Resolution,
    /// When this entry was last written.
    pub updated_at: DateTime<Utc>,
}

impl KnowledgeEntry {
    /// The text used for embedding, identical in form to [`crate::Ticket::searchable_text`].
    #[must_use]
    pub fn searchable_text(&self) -> String {
        searchable_text(&self.title, &self.description)
    }
}

/// A previously resolved ticket supplied to seed the knowledge base,
/// for example exported from an existing helpdesk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoricalTicket {
    /// Ticket title.
    pub title: String,
    /// Ticket description.
    #[serde(default)]
    pub description: String,
    /// Problem category.
    #[serde(default)]
    pub category: Option<Category>,
    /// Severity it was handled at.
    pub severity: Severity,
    /// How it was fixed.
    pub resolution: Resolution,
}

impl HistoricalTicket {
    /// Checks the entry is usable as knowledge.
    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() {
            return Err(Error::Validation(
                "historical ticket title must not be empty".into(),
            ));
        }
        if self.resolution.summary.trim().is_empty() {
            return Err(Error::Validation(format!(
                "historical ticket `{}` has an empty resolution summary",
                self.title
            )));
        }
        Ok(())
    }

    /// Converts into a knowledge entry with a fresh id.
    #[must_use]
    pub fn into_entry(self, now: DateTime<Utc>) -> KnowledgeEntry {
        KnowledgeEntry {
            ticket_id: TicketId::new(),
            title: self.title,
            description: self.description,
            category: self.category,
            severity: self.severity,
            resolution: self.resolution,
            updated_at: now,
        }
    }
}

/// A knowledge-base hit for an incoming ticket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimilarTicket {
    /// The matching historical entry.
    pub entry: KnowledgeEntry,
    /// Cosine similarity between the incoming ticket and the entry.
    pub score: f32,
}
