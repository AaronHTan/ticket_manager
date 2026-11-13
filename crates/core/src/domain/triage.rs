use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Category, Severity, SimilarTicket, TicketId, TicketStatus};

/// The classifier's severity verdict and why it reached it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeverityAssessment {
    /// Assessed severity.
    pub severity: Severity,
    /// Heuristic confidence in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Readable reasons, kept for auditability.
    pub rationale: Vec<String>,
}

/// Output of a [`Classifier`](crate::ports::Classifier).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Classification {
    /// Severity verdict.
    pub severity: SeverityAssessment,
    /// Inferred problem category, used to pick the right expert.
    pub category: Option<Category>,
}

/// A person or group tickets can be routed to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Assignee {
    /// Display name.
    pub name: String,
    /// Email, Slack handle or similar, used by notifiers.
    #[serde(default)]
    pub contact: Option<String>,
    /// Identifier of this assignee in external trackers, such as a Jira `accountId`.
    #[serde(default)]
    pub external_id: Option<String>,
}

/// What the engine decided to do with a ticket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RoutingDecision {
    /// A known, automatable fix exists; apply it without a human.
    AutoResolve {
        /// The historical ticket whose fix is being reused.
        matched: TicketId,
        /// Similarity to that ticket.
        score: f32,
        /// The fix posted back to the reporter.
        resolution: String,
    },
    /// Known problem, but a human should apply the fix.
    Forward {
        /// Intermediate support assignee.
        assignee: Assignee,
        /// The most similar historical ticket.
        matched: TicketId,
        /// Similarity to that ticket.
        score: f32,
        /// The fix that worked last time.
        suggested_resolution: String,
        /// Why it was forwarded rather than auto-resolved.
        reason: String,
    },
    /// Novel or high-risk; needs an expert.
    Escalate {
        /// Subject-matter expert.
        assignee: Assignee,
        /// Why it was escalated.
        reason: String,
    },
}

impl RoutingDecision {
    /// The ticket status this decision moves the ticket into.
    #[must_use]
    pub const fn resulting_status(&self) -> TicketStatus {
        match self {
            Self::AutoResolve { .. } => TicketStatus::AutoResolved,
            Self::Forward { .. } => TicketStatus::Forwarded,
            Self::Escalate { .. } => TicketStatus::Escalated,
        }
    }

    /// The human the ticket was routed to, if any.
    #[must_use]
    pub const fn assignee(&self) -> Option<&Assignee> {
        match self {
            Self::AutoResolve { .. } => None,
            Self::Forward { assignee, .. } | Self::Escalate { assignee, .. } => Some(assignee),
        }
    }
}

/// A compact view of a knowledge-base hit, stored with the triage report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchSummary {
    /// Matched historical ticket.
    pub ticket_id: TicketId,
    /// Its title.
    pub title: String,
    /// Similarity score.
    pub score: f32,
}

impl From<&SimilarTicket> for MatchSummary {
    fn from(value: &SimilarTicket) -> Self {
        Self {
            ticket_id: value.entry.ticket_id,
            title: value.entry.title.clone(),
            score: value.score,
        }
    }
}

/// A resolved ticket cited by a [`DraftResolution`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Precedent {
    /// The resolved ticket.
    pub ticket_id: TicketId,
    /// Its title.
    pub title: String,
    /// Similarity to the ticket being drafted for.
    pub score: f32,
    /// How it was resolved.
    pub resolution: String,
}

/// A preliminary resolution for an escalated ticket, for the expert to
/// review, edit and send. Never sent to the reporter on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftResolution {
    /// The draft, as readable text.
    pub text: String,
    /// First steps from the playbook for the ticket's category.
    pub first_steps: Vec<String>,
    /// The closest resolved tickets, best first, even if not similar enough
    /// to forward on.
    pub precedents: Vec<Precedent>,
}

/// The full, auditable outcome of triaging one ticket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriageReport {
    /// Ticket that was triaged.
    pub ticket_id: TicketId,
    /// Severity and category.
    pub classification: Classification,
    /// Knowledge-base hits considered, best first.
    pub matches: Vec<MatchSummary>,
    /// Routing decision.
    pub decision: RoutingDecision,
    /// A preliminary resolution, drafted for escalated tickets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftResolution>,
    /// When the decision was made.
    pub decided_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_serialises_with_action_tag() {
        let decision = RoutingDecision::Escalate {
            assignee: Assignee {
                name: "Security".into(),
                contact: None,
                external_id: None,
            },
            reason: "novel".into(),
        };
        let json = serde_json::to_value(&decision).unwrap();
        assert_eq!(json["action"], "escalate");
        assert_eq!(decision.resulting_status(), TicketStatus::Escalated);
        assert_eq!(decision.assignee().unwrap().name, "Security");
    }
}
