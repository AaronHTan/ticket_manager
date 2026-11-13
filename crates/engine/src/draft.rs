//! Preliminary resolutions for escalated tickets.
//!
//! An escalated ticket goes to an expert because nothing safe and similar
//! enough is known. The expert should still not start from a blank page:
//! [`ResolutionDrafter`] combines the first steps of a per-category playbook
//! with the closest resolved tickets, even those too weak to forward on, into
//! a draft the expert reviews, edits and sends.
//!
//! The drafter is pure and deterministic, like
//! [`TicketRouter`](crate::TicketRouter). An LLM-backed drafter can replace it
//! later, given the same inputs.

use ticket_core::{
    Classification, DraftResolution, Precedent, RoutingDecision, Severity, SimilarTicket, Ticket,
    TicketKind,
};

/// Builds a [`DraftResolution`] for escalated tickets.
#[derive(Debug, Clone)]
pub struct ResolutionDrafter {
    min_score: f32,
    max_precedents: usize,
}

impl Default for ResolutionDrafter {
    fn default() -> Self {
        Self {
            min_score: Self::DEFAULT_MIN_SCORE,
            max_precedents: 3,
        }
    }
}

impl ResolutionDrafter {
    /// Similarity a past ticket needs to be cited. Much lower than the
    /// forward threshold: a loose precedent is still a useful hint.
    pub const DEFAULT_MIN_SCORE: f32 = 0.3;

    /// A draft for `decision`, or `None` unless the ticket was escalated.
    #[must_use]
    pub fn draft(
        &self,
        ticket: &Ticket,
        classification: &Classification,
        matches: &[SimilarTicket],
        decision: &RoutingDecision,
    ) -> Option<DraftResolution> {
        let RoutingDecision::Escalate { assignee, reason } = decision else {
            return None;
        };
        let severity = classification.severity.severity;
        let category = classification.category.as_ref().map(|c| c.as_str());

        let mut first_steps = Vec::new();
        if severity == Severity::Critical {
            first_steps.push(
                "Treat this as an incident: page the on-call owner and post a first status update within 15 minutes."
                    .to_owned(),
            );
        }
        first_steps.extend(
            playbook(ticket.kind, category)
                .iter()
                .map(|s| (*s).to_owned()),
        );

        let precedents: Vec<Precedent> = matches
            .iter()
            .filter(|m| m.score >= self.min_score)
            .take(self.max_precedents)
            .map(|m| Precedent {
                ticket_id: m.entry.ticket_id,
                title: m.entry.title.clone(),
                score: m.score,
                resolution: m.entry.resolution.summary.clone(),
            })
            .collect();

        let mut text = format!(
            "Preliminary assessment for \"{}\" (draft, for expert review)\n\n\
             Severity: {severity}, confidence {:.0}%. {}\n\
             Routed to {}: {reason}.\n\n\
             Suggested first steps:\n",
            ticket.title,
            classification.severity.confidence * 100.0,
            classification
                .severity
                .rationale
                .first()
                .map(|r| format!("Main signal: {r}."))
                .unwrap_or_default(),
            assignee.name,
        );
        for (i, step) in first_steps.iter().enumerate() {
            text.push_str(&format!("{}. {step}\n", i + 1));
        }
        text.push_str("\nSimilar past cases:\n");
        if precedents.is_empty() {
            text.push_str("- None close enough to rely on. This looks new.\n");
        }
        for p in &precedents {
            text.push_str(&format!(
                "- \"{}\" ({:.0}% similar): {}\n",
                p.title,
                p.score * 100.0,
                p.resolution
            ));
        }

        Some(DraftResolution {
            text,
            first_steps,
            precedents,
        })
    }
}

/// First steps per kind of request, then per problem category. Deliberately
/// generic: they orient the expert, and the precedents supply the specifics.
fn playbook(kind: TicketKind, category: Option<&str>) -> &'static [&'static str] {
    match kind {
        TicketKind::FeatureRequest => {
            return &[
                "Clarify the use case: who is asking, and what they are trying to do.",
                "Check the roadmap and existing requests for the same need.",
                "Reply with a decision or timeline, and a workaround if there is one.",
            ];
        }
        TicketKind::Question => {
            return &[
                "Answer from the documentation if it covers this, and link to it.",
                "If the documentation is missing or wrong, file a docs fix.",
            ];
        }
        _ => {}
    }
    match category {
        Some("security") => &[
            "Contain first: revoke or rotate any exposed credentials, keys and sessions.",
            "Review access and audit logs for the affected accounts since the first sign of trouble.",
            "Preserve evidence (logs, snapshots) before changing anything.",
            "Agree with the security lead whether customers must be notified.",
        ],
        Some("database") => &[
            "Check primary and replica health, connection-pool usage and replication lag.",
            "Look for long-running or blocking queries and recent schema or index changes.",
            "Scale or fail over only once the bottleneck is confirmed.",
        ],
        Some("billing") => &[
            "Confirm the affected transactions in the payment processor.",
            "Void duplicates or issue refunds according to policy.",
            "Tell the customer what was done and when the money will arrive.",
        ],
        Some("authentication") => &[
            "Check the identity provider's status and any recent configuration or certificate changes.",
            "Reproduce with a test account.",
            "Unlock or reset the affected accounts if needed.",
        ],
        Some("performance") => &[
            "Check latency and error dashboards and correlate spikes with recent deploys.",
            "Roll back the latest deploy if it lines up with the start of the problem.",
            "Scale the saturated tier if capacity is the cause.",
        ],
        _ => &[
            "Confirm the impact: who is affected, since when, and how badly.",
            "Check recent deploys and configuration changes, and roll back if one lines up.",
            "Gather logs or a reproduction, then decide on a fix or a workaround.",
        ],
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ticket_core::{
        Assignee, Category, ClientId, KnowledgeEntry, NewTicket, Resolution, SeverityAssessment,
        TicketId,
    };

    use super::*;

    fn classification(severity: Severity, category: Option<&str>) -> Classification {
        Classification {
            severity: SeverityAssessment {
                severity,
                confidence: 0.8,
                rationale: vec!["keyword `breach` suggests critical".into()],
            },
            category: category.map(|c| Category::new(c).unwrap()),
        }
    }

    fn hit(title: &str, score: f32) -> SimilarTicket {
        SimilarTicket {
            entry: KnowledgeEntry {
                ticket_id: TicketId::new(),
                title: title.into(),
                description: String::new(),
                category: None,
                severity: Severity::High,
                resolution: Resolution {
                    summary: format!("fix for {title}"),
                    automatable: false,
                    resolved_by: None,
                    patch: None,
                },
                updated_at: Utc::now(),
            },
            score,
        }
    }

    fn escalate() -> RoutingDecision {
        RoutingDecision::Escalate {
            assignee: Assignee {
                name: "Security Team".into(),
                contact: None,
                external_id: None,
            },
            reason: "severity critical always requires an expert".into(),
        }
    }

    fn ticket() -> Ticket {
        Ticket::receive(
            NewTicket::new(ClientId::new("acme").unwrap(), "Admin logins", "breach"),
            Utc::now(),
        )
        .unwrap()
    }

    #[test]
    fn only_escalations_get_a_draft() {
        let forward = RoutingDecision::Forward {
            assignee: Assignee {
                name: "L2".into(),
                contact: None,
                external_id: None,
            },
            matched: TicketId::new(),
            score: 0.8,
            suggested_resolution: "x".into(),
            reason: "similar".into(),
        };
        let drafter = ResolutionDrafter::default();
        let c = classification(Severity::High, None);
        assert!(drafter.draft(&ticket(), &c, &[], &forward).is_none());
        assert!(drafter.draft(&ticket(), &c, &[], &escalate()).is_some());
    }

    #[test]
    fn critical_security_draft_has_incident_and_playbook_steps_and_precedents() {
        let matches = [
            hit("API key leaked", 0.45),
            hit("Phishing email", 0.3),
            hit("Word overlap only", 0.23),
            hit("Unrelated", 0.05),
        ];
        let draft = ResolutionDrafter::default()
            .draft(
                &ticket(),
                &classification(Severity::Critical, Some("security")),
                &matches,
                &escalate(),
            )
            .unwrap();
        assert!(draft.first_steps[0].starts_with("Treat this as an incident"));
        assert!(draft.first_steps[1].starts_with("Contain first"));
        assert_eq!(
            draft
                .precedents
                .iter()
                .map(|p| p.title.as_str())
                .collect::<Vec<_>>(),
            ["API key leaked", "Phishing email"],
            "weak matches are dropped"
        );
        assert!(draft.text.contains("Routed to Security Team"));
        assert!(
            draft
                .text
                .contains("\"API key leaked\" (45% similar): fix for API key leaked")
        );
        assert!(
            draft
                .text
                .contains("Main signal: keyword `breach` suggests critical.")
        );
    }

    #[test]
    fn feature_requests_get_product_steps_not_incident_steps() {
        let mut request = ticket();
        request.kind = TicketKind::FeatureRequest;
        let draft = ResolutionDrafter::default()
            .draft(
                &request,
                &classification(Severity::Medium, None),
                &[],
                &escalate(),
            )
            .unwrap();
        assert!(draft.first_steps[0].starts_with("Clarify the use case"));
        assert!(!draft.text.contains("roll back"));
    }

    #[test]
    fn novel_ticket_draft_says_so_and_uses_the_generic_playbook() {
        let draft = ResolutionDrafter::default()
            .draft(
                &ticket(),
                &classification(Severity::Low, None),
                &[hit("Unrelated", 0.1)],
                &escalate(),
            )
            .unwrap();
        assert!(draft.precedents.is_empty());
        assert!(draft.text.contains("This looks new."));
        assert!(draft.first_steps[0].starts_with("Confirm the impact"));
    }
}
