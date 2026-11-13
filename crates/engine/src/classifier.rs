//! Severity and category classification.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ticket_core::ports::Classifier;
use ticket_core::{
    Category, Classification, Result, Severity, SeverityAssessment, SimilarTicket, Ticket,
};

/// A keyword-triggered hint. If any keyword appears in the ticket text as a
/// whole word or phrase, the rule adds its severity and/or category.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordRule {
    /// Words or phrases to match, case-insensitively, on word boundaries.
    pub keywords: Vec<String>,
    /// Severity suggested when the rule matches.
    #[serde(default)]
    pub severity: Option<Severity>,
    /// Category suggested when the rule matches.
    #[serde(default)]
    pub category: Option<Category>,
}

impl KeywordRule {
    fn new(keywords: &[&str], severity: Option<Severity>, category: Option<&str>) -> Self {
        Self {
            keywords: keywords.iter().map(|k| (*k).to_owned()).collect(),
            severity,
            category: category.map(|c| Category::new(c).expect("built-in category is valid")),
        }
    }
}

/// A transparent, rule-based classifier.
///
/// It combines three signals and takes the **maximum** severity among them,
/// because under-estimating severity costs more than over-estimating it:
///
/// 1. keyword rules matched against the title and description,
/// 2. the severity reported by the upstream system,
/// 3. a similarity-weighted vote over the severities of matching past tickets.
///
/// Category comes from the first matching keyword rule, falling back to the
/// best sufficiently similar past ticket.
///
/// An LLM-backed classifier can replace this by implementing the same
/// [`Classifier`] port. This one remains useful as a fallback and as a baseline.
#[derive(Debug, Clone)]
pub struct RuleBasedClassifier {
    rules: Vec<NormalisedRule>,
    neighbour_min_score: f32,
}

#[derive(Debug, Clone)]
struct NormalisedRule {
    /// Each keyword normalised to ` word word ` form for boundary-safe matching.
    patterns: Vec<String>,
    severity: Option<Severity>,
    category: Option<Category>,
}

impl RuleBasedClassifier {
    /// Default similarity a past ticket needs before its severity and category count.
    pub const DEFAULT_NEIGHBOUR_MIN_SCORE: f32 = 0.5;

    /// Creates a classifier from rules. Earlier rules take precedence for category.
    #[must_use]
    pub fn new(rules: Vec<KeywordRule>) -> Self {
        let rules = rules
            .into_iter()
            .map(|rule| NormalisedRule {
                patterns: rule
                    .keywords
                    .iter()
                    .map(|k| normalise(k))
                    .filter(|k| !k.trim().is_empty())
                    .collect(),
                severity: rule.severity,
                category: rule.category,
            })
            .collect();
        Self {
            rules,
            neighbour_min_score: Self::DEFAULT_NEIGHBOUR_MIN_SCORE,
        }
    }

    /// Sets the minimum similarity for a past ticket to influence the result.
    #[must_use]
    pub fn with_neighbour_min_score(mut self, score: f32) -> Self {
        self.neighbour_min_score = score;
        self
    }

    /// A built-in starter rule set. Expect to tune it per client.
    #[must_use]
    pub fn default_rules() -> Vec<KeywordRule> {
        use Severity::{Critical, High, Low, Medium};
        vec![
            KeywordRule::new(
                &[
                    "security",
                    "breach",
                    "vulnerability",
                    "leaked",
                    "unauthorized access",
                    "phishing",
                ],
                Some(Critical),
                Some("security"),
            ),
            KeywordRule::new(
                &[
                    "outage",
                    "production down",
                    "site down",
                    "all users",
                    "data loss",
                    "sev1",
                ],
                Some(Critical),
                None,
            ),
            KeywordRule::new(
                &[
                    "database",
                    "deadlock",
                    "replication",
                    "connection pool",
                    "query timeout",
                ],
                Some(High),
                Some("database"),
            ),
            KeywordRule::new(
                &[
                    "login",
                    "password",
                    "sso",
                    "mfa",
                    "2fa",
                    "authentication",
                    "locked out",
                ],
                None,
                Some("authentication"),
            ),
            KeywordRule::new(
                &[
                    "invoice",
                    "billing",
                    "charged",
                    "refund",
                    "payment",
                    "subscription",
                ],
                Some(Medium),
                Some("billing"),
            ),
            KeywordRule::new(
                &["slow", "latency", "performance", "timeout"],
                Some(Medium),
                Some("performance"),
            ),
            KeywordRule::new(
                &["crash", "crashes", "exception", "stack trace", "500"],
                Some(High),
                None,
            ),
            KeywordRule::new(
                &["error", "failed", "fails", "broken", "unable", "cannot"],
                Some(Medium),
                None,
            ),
            KeywordRule::new(
                &[
                    "typo",
                    "cosmetic",
                    "question",
                    "how do i",
                    "feature request",
                ],
                Some(Low),
                None,
            ),
        ]
    }

    /// Classifies synchronously. The async trait method delegates here.
    #[must_use]
    pub fn classify_ticket(&self, ticket: &Ticket, matches: &[SimilarTicket]) -> Classification {
        let text = normalise(&format!("{} {}", ticket.title, ticket.description));
        let mut rationale = Vec::new();
        let mut keyword_severity: Option<Severity> = None;
        let mut category: Option<Category> = None;

        for rule in &self.rules {
            let Some(hit) = rule.patterns.iter().find(|p| text.contains(p.as_str())) else {
                continue;
            };
            let hit = hit.trim();
            if let Some(severity) = rule.severity {
                if keyword_severity.is_none_or(|current| severity > current) {
                    keyword_severity = Some(severity);
                }
                rationale.push(format!("keyword `{hit}` suggests {severity}"));
            }
            if category.is_none()
                && let Some(c) = &rule.category
            {
                rationale.push(format!("keyword `{hit}` suggests category `{c}`"));
                category = Some(c.clone());
            }
        }

        let neighbour = self.neighbour_vote(matches);
        if let Some((severity, weight)) = neighbour {
            rationale.push(format!(
                "similar past tickets suggest {severity} (weight {weight:.2})"
            ));
        }
        if category.is_none()
            && let Some(m) = matches
                .iter()
                .filter(|m| m.score >= self.neighbour_min_score)
                .find(|m| m.entry.category.is_some())
        {
            category = m.entry.category.clone();
            rationale.push(format!(
                "category taken from similar ticket `{}` ({:.2})",
                m.entry.title, m.score
            ));
        }
        if let Some(reported) = ticket.reported_severity {
            rationale.push(format!("reporter marked {reported}"));
        }

        let signals: Vec<Severity> = [
            keyword_severity,
            ticket.reported_severity,
            neighbour.map(|(s, _)| s),
        ]
        .into_iter()
        .flatten()
        .collect();

        let (severity, confidence) = match signals.iter().max() {
            None => {
                rationale.push("no severity signals; defaulting to medium".into());
                (Severity::Medium, 0.2)
            }
            Some(&max) => {
                let agreeing = signals.iter().filter(|s| **s == max).count();
                let agreement = agreeing as f32 / signals.len() as f32;
                (max, (0.4 + 0.5 * agreement).min(1.0))
            }
        };

        Classification {
            severity: SeverityAssessment {
                severity,
                confidence,
                rationale,
            },
            category,
        }
    }

    /// Similarity-weighted severity vote. Ties go to the higher severity.
    fn neighbour_vote(&self, matches: &[SimilarTicket]) -> Option<(Severity, f32)> {
        let mut weights = [0.0_f32; Severity::ALL.len()];
        for m in matches
            .iter()
            .filter(|m| m.score >= self.neighbour_min_score)
        {
            weights[m.entry.severity.index()] += m.score;
        }
        Severity::ALL
            .into_iter()
            .zip(weights)
            .filter(|(_, w)| *w > 0.0)
            // `max_by` keeps the last of equal elements, so ties favour the higher severity.
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

impl Default for RuleBasedClassifier {
    fn default() -> Self {
        Self::new(Self::default_rules())
    }
}

#[async_trait]
impl Classifier for RuleBasedClassifier {
    async fn classify(&self, ticket: &Ticket, matches: &[SimilarTicket]) -> Result<Classification> {
        Ok(self.classify_ticket(ticket, matches))
    }
}

/// Lower-cases and collapses text to ` tok tok tok ` so that a phrase lookup
/// with `contains` only matches on word boundaries.
fn normalise(text: &str) -> String {
    let mut out = String::from(" ");
    for token in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        out.push_str(&token.to_lowercase());
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ticket_core::{ClientId, KnowledgeEntry, NewTicket, Resolution, TicketId};

    use super::*;

    fn ticket(title: &str, description: &str, reported: Option<Severity>) -> Ticket {
        let mut new = NewTicket::new(ClientId::new("acme").unwrap(), title, description);
        new.reported_severity = reported;
        Ticket::receive(new, Utc::now()).unwrap()
    }

    fn similar(severity: Severity, category: Option<&str>, score: f32) -> SimilarTicket {
        SimilarTicket {
            entry: KnowledgeEntry {
                ticket_id: TicketId::new(),
                title: "past".into(),
                description: String::new(),
                category: category.map(|c| Category::new(c).unwrap()),
                severity,
                resolution: Resolution {
                    summary: "fix".into(),
                    automatable: false,
                    resolved_by: None,
                    patch: None,
                },
                updated_at: Utc::now(),
            },
            score,
        }
    }

    #[test]
    fn keywords_match_on_word_boundaries_only() {
        let c = RuleBasedClassifier::default();
        // "downloading" must not trigger "down"-style rules, and "sso" is not in "lesson".
        let result = c.classify_ticket(&ticket("Lesson downloading", "nothing else", None), &[]);
        assert_eq!(result.category, None);
        assert_eq!(result.severity.severity, Severity::Medium);
        assert!(result.severity.confidence < 0.3);
    }

    #[test]
    fn security_keywords_are_critical_with_category() {
        let c = RuleBasedClassifier::default();
        let result = c.classify_ticket(
            &ticket(
                "Possible security breach",
                "Unauthorized access to admin panel",
                None,
            ),
            &[],
        );
        assert_eq!(result.severity.severity, Severity::Critical);
        assert_eq!(result.category.unwrap().as_str(), "security");
    }

    #[test]
    fn highest_signal_wins() {
        let c = RuleBasedClassifier::default();
        // Keyword says low ("question") but the reporter says high.
        let result = c.classify_ticket(
            &ticket("Question about exports", "", Some(Severity::High)),
            &[],
        );
        assert_eq!(result.severity.severity, Severity::High);
    }

    #[test]
    fn neighbours_vote_on_severity_and_category() {
        let c = RuleBasedClassifier::default();
        let matches = [
            similar(Severity::Low, Some("onboarding"), 0.9),
            similar(Severity::High, None, 0.55),
            similar(Severity::Critical, None, 0.3), // below the threshold: ignored
        ];
        let result = c.classify_ticket(&ticket("Welcome email", "", None), &matches);
        assert_eq!(result.severity.severity, Severity::Low);
        assert_eq!(result.category.unwrap().as_str(), "onboarding");
    }

    #[test]
    fn neighbour_ties_favour_higher_severity() {
        let c = RuleBasedClassifier::default();
        let matches = [
            similar(Severity::Low, None, 0.8),
            similar(Severity::High, None, 0.8),
        ];
        assert_eq!(
            c.neighbour_vote(&matches).map(|(s, _)| s),
            Some(Severity::High)
        );
    }

    #[test]
    fn agreement_raises_confidence() {
        let c = RuleBasedClassifier::default();
        let agree = c.classify_ticket(&ticket("App crash", "", Some(Severity::High)), &[]);
        let disagree = c.classify_ticket(&ticket("App crash", "", Some(Severity::Low)), &[]);
        assert!(agree.severity.confidence > disagree.severity.confidence);
    }
}
