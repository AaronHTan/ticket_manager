use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Business impact of a ticket.
///
/// The derived ordering is meaningful: `Low < Medium < High < Critical`.
/// Routing policies compare severities directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Cosmetic issues and questions.
    Low,
    /// Degraded functionality with a workaround.
    Medium,
    /// Major functionality broken for some users.
    High,
    /// Outage, data loss or security incident.
    Critical,
}

impl Severity {
    /// Every severity, in ascending order.
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Critical];

    /// Stable string form, used for storage and over the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// Zero-based position in [`Severity::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Severity {
    type Err = Error;

    /// Parses a severity case-insensitively.
    fn from_str(s: &str) -> Result<Self> {
        let needle = s.trim();
        Self::ALL
            .into_iter()
            .find(|sev| sev.as_str().eq_ignore_ascii_case(needle))
            .ok_or_else(|| Error::Validation(format!("unknown severity `{s}`")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severities_are_ordered_by_impact() {
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
        assert!(Severity::High < Severity::Critical);
        assert_eq!(Severity::ALL.iter().max(), Some(&Severity::Critical));
    }

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!("HIGH".parse::<Severity>().unwrap(), Severity::High);
        assert_eq!(
            " critical ".parse::<Severity>().unwrap(),
            Severity::Critical
        );
        assert!("urgent".parse::<Severity>().is_err());
    }

    #[test]
    fn index_matches_position_in_all() {
        for (i, sev) in Severity::ALL.into_iter().enumerate() {
            assert_eq!(sev.index(), i);
        }
    }
}
