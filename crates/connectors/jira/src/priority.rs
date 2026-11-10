use ticket_core::Severity;

/// Maps a Jira priority name to a severity. Covers Jira's defaults and common
/// custom schemes. Returns `None` for unknown names.
#[must_use]
pub fn severity_from_priority(name: &str) -> Option<Severity> {
    match name.trim().to_ascii_lowercase().as_str() {
        "highest" | "blocker" | "critical" | "p1" => Some(Severity::Critical),
        "high" | "major" | "p2" => Some(Severity::High),
        "medium" | "normal" | "p3" => Some(Severity::Medium),
        "low" | "lowest" | "minor" | "trivial" | "p4" | "p5" => Some(Severity::Low),
        _ => None,
    }
}

/// The default Jira priority name for a severity.
#[must_use]
pub const fn priority_for_severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "Highest",
        Severity::High => "High",
        Severity::Medium => "Medium",
        Severity::Low => "Low",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_priorities_round_trip() {
        for severity in Severity::ALL {
            assert_eq!(
                severity_from_priority(priority_for_severity(severity)),
                Some(severity)
            );
        }
        assert_eq!(severity_from_priority("Blocker"), Some(Severity::Critical));
        assert_eq!(severity_from_priority("whenever"), None);
    }
}
