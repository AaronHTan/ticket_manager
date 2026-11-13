use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::{Error, Result};

/// The person who raised a ticket.
///
/// Upstream systems identify people differently (Jira has an `accountId`, an
/// email form has only an address), so every field is optional. At least one
/// must be present.
///
/// For backward compatibility a reporter also deserialises from a plain
/// string. A string containing `@` is taken as the email, anything else as
/// the name:
///
/// ```
/// # use ticket_core::Reporter;
/// let r: Reporter = serde_json::from_str("\"jane@acme.test\"").unwrap();
/// assert_eq!(r.email.as_deref(), Some("jane@acme.test"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Reporter {
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Identifier in the originating system, such as a Jira `accountId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

impl Reporter {
    /// Maximum length of each field, in bytes.
    pub const MAX_FIELD_LEN: usize = 256;

    /// Builds a reporter from a free-form label (an email or a display name).
    pub fn from_label(label: impl Into<String>) -> Result<Self> {
        let label = label.into().trim().to_owned();
        let reporter = if label.contains('@') {
            Self {
                name: None,
                email: Some(label),
                external_id: None,
            }
        } else {
            Self {
                name: Some(label),
                email: None,
                external_id: None,
            }
        };
        reporter.validate()?;
        Ok(reporter)
    }

    /// Checks that at least one field is set and that none is oversized.
    pub fn validate(&self) -> Result<()> {
        let fields = [&self.name, &self.email, &self.external_id];
        if fields
            .iter()
            .all(|f| f.as_deref().is_none_or(|s| s.trim().is_empty()))
        {
            return Err(Error::Validation(
                "reporter needs a name, email or external id".into(),
            ));
        }
        if fields
            .iter()
            .any(|f| f.as_deref().is_some_and(|s| s.len() > Self::MAX_FIELD_LEN))
        {
            return Err(Error::Validation(format!(
                "reporter fields must be at most {} bytes",
                Self::MAX_FIELD_LEN
            )));
        }
        Ok(())
    }

    /// The most useful single identifier: email, then name, then external id.
    #[must_use]
    pub fn label(&self) -> &str {
        self.email
            .as_deref()
            .or(self.name.as_deref())
            .or(self.external_id.as_deref())
            .unwrap_or_default()
    }
}

impl fmt::Display for Reporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl<'de> Deserialize<'de> for Reporter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            email: Option<String>,
            #[serde(default)]
            external_id: Option<String>,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Label(String),
            Fields(Fields),
        }

        let reporter = match Repr::deserialize(deserializer)? {
            Repr::Label(label) => return Self::from_label(label).map_err(serde::de::Error::custom),
            Repr::Fields(f) => Self {
                name: f.name,
                email: f.email,
                external_id: f.external_id,
            },
        };
        reporter.validate().map_err(serde::de::Error::custom)?;
        Ok(reporter)
    }
}

/// What sort of request a ticket is.
///
/// Drives later stages: only some kinds (bugs, incidents) are candidates for
/// automated code changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketKind {
    /// Something does not work as intended.
    Bug,
    /// A live production problem.
    Incident,
    /// A request for new behaviour.
    FeatureRequest,
    /// Planned work.
    Task,
    /// A request for information.
    Question,
    /// Unknown or unmapped.
    #[default]
    Other,
}

impl TicketKind {
    /// Every kind.
    pub const ALL: [Self; 6] = [
        Self::Bug,
        Self::Incident,
        Self::FeatureRequest,
        Self::Task,
        Self::Question,
        Self::Other,
    ];

    /// Stable string form, used for storage and over the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Incident => "incident",
            Self::FeatureRequest => "feature_request",
            Self::Task => "task",
            Self::Question => "question",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for TicketKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TicketKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| Error::Validation(format!("unknown ticket kind `{s}`")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reporter_deserialises_from_string_or_object() {
        let email: Reporter = serde_json::from_str("\"jane@acme.test\"").unwrap();
        assert_eq!(email.email.as_deref(), Some("jane@acme.test"));
        assert_eq!(email.name, None);

        let name: Reporter = serde_json::from_str("\"Jane Doe\"").unwrap();
        assert_eq!(name.name.as_deref(), Some("Jane Doe"));

        let full: Reporter = serde_json::from_str(
            r#"{"name":"Jane Doe","email":"jane@acme.test","external_id":"5b10ac"}"#,
        )
        .unwrap();
        assert_eq!(full.external_id.as_deref(), Some("5b10ac"));
        assert_eq!(full.label(), "jane@acme.test");

        let round_trip: Reporter =
            serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!(round_trip, full);
    }

    #[test]
    fn reporter_rejects_empty_and_oversized() {
        assert!(serde_json::from_str::<Reporter>("\"  \"").is_err());
        assert!(serde_json::from_str::<Reporter>("{}").is_err());
        assert!(serde_json::from_str::<Reporter>(r#"{"name":""}"#).is_err());
        assert!(Reporter::from_label("x".repeat(257)).is_err());
        assert!(serde_json::from_str::<Reporter>(r#"{"nmae":"typo"}"#).is_err());
    }

    #[test]
    fn reporter_label_prefers_email_then_name_then_id() {
        let id_only = Reporter {
            name: None,
            email: None,
            external_id: Some("abc".into()),
        };
        assert_eq!(id_only.label(), "abc");
        let named = Reporter {
            name: Some("Jane".into()),
            ..id_only
        };
        assert_eq!(named.to_string(), "Jane");
    }

    #[test]
    fn ticket_kind_round_trips_through_strings() {
        for kind in TicketKind::ALL {
            assert_eq!(kind.as_str().parse::<TicketKind>().unwrap(), kind);
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
        assert_eq!(TicketKind::default(), TicketKind::Other);
        assert!("bogus".parse::<TicketKind>().is_err());
    }
}
