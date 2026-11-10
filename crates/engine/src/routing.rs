//! Turning a classification and its knowledge-base matches into a decision.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ticket_core::{
    Assignee, Category, Classification, Error, Result, RoutingDecision, Severity, SimilarTicket,
};

/// Thresholds that decide between auto-resolving, forwarding and escalating.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingPolicy {
    /// Minimum similarity to an automatable resolved ticket to auto-resolve.
    pub auto_resolve_threshold: f32,
    /// Minimum similarity for a ticket to count as "known" and be forwarded
    /// to intermediate support with a suggested fix.
    pub forward_threshold: f32,
    /// Tickets above this severity are never auto-resolved, however similar.
    pub max_auto_resolve_severity: Severity,
    /// Tickets at or above this severity always go to an expert.
    pub escalate_severity: Severity,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            auto_resolve_threshold: 0.90,
            forward_threshold: 0.65,
            max_auto_resolve_severity: Severity::Medium,
            escalate_severity: Severity::Critical,
        }
    }
}

impl RoutingPolicy {
    /// Rejects inconsistent thresholds at start-up rather than at triage time.
    pub fn validate(&self) -> Result<()> {
        let in_range = |v: f32| (0.0..=1.0).contains(&v);
        if !in_range(self.auto_resolve_threshold) || !in_range(self.forward_threshold) {
            return Err(Error::Config(
                "routing thresholds must be within [0, 1]".into(),
            ));
        }
        if self.forward_threshold > self.auto_resolve_threshold {
            return Err(Error::Config(
                "forward_threshold must not exceed auto_resolve_threshold".into(),
            ));
        }
        Ok(())
    }
}

/// Who receives forwarded and escalated tickets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalationDirectory {
    /// Intermediate (L2) support. Receives known problems that need a human.
    pub intermediate: Assignee,
    /// Expert used when no category-specific expert is configured.
    pub default_expert: Assignee,
    /// Experts by category.
    #[serde(default)]
    pub experts: HashMap<Category, Assignee>,
}

impl EscalationDirectory {
    /// The expert responsible for `category`.
    #[must_use]
    pub fn expert_for(&self, category: Option<&Category>) -> &Assignee {
        category
            .and_then(|c| self.experts.get(c))
            .unwrap_or(&self.default_expert)
    }
}

/// Makes routing decisions. Pure: no I/O and fully determined by its inputs,
/// so it is cheap to unit-test exhaustively.
#[derive(Debug, Clone)]
pub struct TicketRouter {
    policy: RoutingPolicy,
    directory: EscalationDirectory,
}

impl TicketRouter {
    /// Creates a router after validating the policy.
    pub fn new(policy: RoutingPolicy, directory: EscalationDirectory) -> Result<Self> {
        policy.validate()?;
        Ok(Self { policy, directory })
    }

    /// The active policy.
    #[must_use]
    pub fn policy(&self) -> &RoutingPolicy {
        &self.policy
    }

    /// Decides what to do with a ticket.
    #[must_use]
    pub fn decide(
        &self,
        classification: &Classification,
        matches: &[SimilarTicket],
    ) -> RoutingDecision {
        let severity = classification.severity.severity;
        let policy = &self.policy;
        let best = matches.iter().max_by(|a, b| a.score.total_cmp(&b.score));
        let expert = || {
            self.directory
                .expert_for(classification.category.as_ref())
                .clone()
        };

        if severity >= policy.escalate_severity {
            return RoutingDecision::Escalate {
                assignee: expert(),
                reason: format!("severity {severity} always requires an expert"),
            };
        }

        match best {
            Some(m)
                if m.score >= policy.auto_resolve_threshold
                    && m.entry.resolution.automatable
                    && severity <= policy.max_auto_resolve_severity =>
            {
                RoutingDecision::AutoResolve {
                    matched: m.entry.ticket_id,
                    score: m.score,
                    resolution: m.entry.resolution.summary.clone(),
                }
            }
            Some(m) if m.score >= policy.forward_threshold => {
                let reason = if m.score < policy.auto_resolve_threshold {
                    format!("similar to `{}` (score {:.2})", m.entry.title, m.score)
                } else if !m.entry.resolution.automatable {
                    format!(
                        "matches `{}` (score {:.2}) but its fix is not marked automatable",
                        m.entry.title, m.score
                    )
                } else {
                    format!(
                        "matches `{}` (score {:.2}) but severity {severity} requires human review",
                        m.entry.title, m.score
                    )
                };
                RoutingDecision::Forward {
                    assignee: self.directory.intermediate.clone(),
                    matched: m.entry.ticket_id,
                    score: m.score,
                    suggested_resolution: m.entry.resolution.summary.clone(),
                    reason,
                }
            }
            Some(m) => RoutingDecision::Escalate {
                assignee: expert(),
                reason: format!(
                    "novel: closest match scored {:.2}, below forward threshold {:.2}",
                    m.score, policy.forward_threshold
                ),
            },
            None => RoutingDecision::Escalate {
                assignee: expert(),
                reason: "novel: knowledge base has no comparable tickets".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ticket_core::{KnowledgeEntry, Resolution, SeverityAssessment, TicketId};

    use super::*;

    fn assignee(name: &str) -> Assignee {
        Assignee {
            name: name.into(),
            contact: None,
            external_id: None,
        }
    }

    fn router() -> TicketRouter {
        let directory = EscalationDirectory {
            intermediate: assignee("L2"),
            default_expert: assignee("Platform"),
            experts: HashMap::from([(Category::new("database").unwrap(), assignee("DBA"))]),
        };
        TicketRouter::new(RoutingPolicy::default(), directory).unwrap()
    }

    fn classification(severity: Severity, category: Option<&str>) -> Classification {
        Classification {
            severity: SeverityAssessment {
                severity,
                confidence: 0.8,
                rationale: vec![],
            },
            category: category.map(|c| Category::new(c).unwrap()),
        }
    }

    fn hit(score: f32, automatable: bool) -> SimilarTicket {
        SimilarTicket {
            entry: KnowledgeEntry {
                ticket_id: TicketId::new(),
                title: "Password reset".into(),
                description: String::new(),
                category: None,
                severity: Severity::Low,
                resolution: Resolution {
                    summary: "Resend the reset link".into(),
                    automatable,
                    resolved_by: None,
                },
                updated_at: Utc::now(),
            },
            score,
        }
    }

    #[test]
    fn near_duplicate_with_automatable_fix_is_auto_resolved() {
        let d = router().decide(&classification(Severity::Low, None), &[hit(0.95, true)]);
        assert!(
            matches!(d, RoutingDecision::AutoResolve { ref resolution, .. } if resolution == "Resend the reset link")
        );
    }

    #[test]
    fn best_match_is_used_regardless_of_order() {
        let d = router().decide(
            &classification(Severity::Low, None),
            &[hit(0.70, false), hit(0.95, true)],
        );
        assert!(matches!(d, RoutingDecision::AutoResolve { .. }));
    }

    #[test]
    fn non_automatable_fix_is_forwarded() {
        let d = router().decide(&classification(Severity::Low, None), &[hit(0.95, false)]);
        assert!(
            matches!(d, RoutingDecision::Forward { ref reason, .. } if reason.contains("not marked automatable"))
        );
    }

    #[test]
    fn high_severity_blocks_auto_resolution() {
        let d = router().decide(&classification(Severity::High, None), &[hit(0.99, true)]);
        assert!(
            matches!(d, RoutingDecision::Forward { ref reason, .. } if reason.contains("requires human review"))
        );
    }

    #[test]
    fn moderately_similar_ticket_is_forwarded_to_intermediate() {
        let d = router().decide(&classification(Severity::Medium, None), &[hit(0.70, true)]);
        assert!(
            matches!(d, RoutingDecision::Forward { ref assignee, .. } if assignee.name == "L2")
        );
    }

    #[test]
    fn novel_ticket_is_escalated_to_category_expert() {
        let d = router().decide(
            &classification(Severity::Medium, Some("database")),
            &[hit(0.3, true)],
        );
        assert!(
            matches!(d, RoutingDecision::Escalate { ref assignee, .. } if assignee.name == "DBA")
        );
        let d = router().decide(&classification(Severity::Medium, Some("unknown")), &[]);
        assert!(
            matches!(d, RoutingDecision::Escalate { ref assignee, .. } if assignee.name == "Platform")
        );
    }

    #[test]
    fn critical_tickets_always_escalate() {
        let d = router().decide(&classification(Severity::Critical, None), &[hit(1.0, true)]);
        assert!(matches!(d, RoutingDecision::Escalate { .. }));
    }

    #[test]
    fn thresholds_on_the_boundary_are_inclusive() {
        let policy = RoutingPolicy::default();
        let d = router().decide(
            &classification(Severity::Low, None),
            &[hit(policy.auto_resolve_threshold, true)],
        );
        assert!(matches!(d, RoutingDecision::AutoResolve { .. }));
        let d = router().decide(
            &classification(Severity::Low, None),
            &[hit(policy.forward_threshold, true)],
        );
        assert!(matches!(d, RoutingDecision::Forward { .. }));
    }

    #[test]
    fn invalid_policies_are_rejected() {
        let inverted = RoutingPolicy {
            auto_resolve_threshold: 0.5,
            forward_threshold: 0.8,
            ..RoutingPolicy::default()
        };
        assert!(inverted.validate().is_err());
        let out_of_range = RoutingPolicy {
            auto_resolve_threshold: 1.5,
            ..RoutingPolicy::default()
        };
        assert!(out_of_range.validate().is_err());
    }
}
