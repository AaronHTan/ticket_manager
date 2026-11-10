//! Parsing and authenticating Jira webhook deliveries.

use std::collections::HashMap;

use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;
use ticket_core::{Error, ExternalRef, NewTicket, Result};

use crate::adf::adf_to_text;
use crate::config::JiraConfig;
use crate::priority::severity_from_priority;

/// Header carrying the webhook signature, as `sha256=<hex digest>`.
pub const SIGNATURE_HEADER: &str = "x-hub-signature";

/// What a webhook delivery means for the handler.
#[derive(Debug, Clone, PartialEq)]
pub enum JiraEvent {
    /// A new issue to triage.
    Created(NewTicket),
    /// An issue was resolved in Jira.
    Resolved {
        /// The resolved issue.
        external_ref: ExternalRef,
        /// The fix, read from the configured resolution field.
        summary: Option<String>,
        /// Assignee at resolution time.
        resolved_by: Option<String>,
    },
    /// An event the handler does not act on, with the reason.
    Ignored(String),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    webhook_event: String,
    issue: Option<Issue>,
}

#[derive(Debug, Deserialize)]
struct Issue {
    key: String,
    fields: Fields,
}

#[derive(Debug, Deserialize)]
struct Fields {
    summary: Option<String>,
    description: Option<Value>,
    priority: Option<Named>,
    labels: Option<Vec<String>>,
    reporter: Option<User>,
    assignee: Option<User>,
    resolution: Option<Named>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
struct Named {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    display_name: Option<String>,
    email_address: Option<String>,
}

impl User {
    fn label(self) -> Option<String> {
        self.email_address.or(self.display_name)
    }
}

/// Parses a webhook body from connector `name`.
pub fn parse_webhook(name: &str, config: &JiraConfig, body: &[u8]) -> Result<JiraEvent> {
    let payload: Payload = serde_json::from_slice(body)
        .map_err(|e| Error::Validation(format!("invalid Jira webhook payload: {e}")))?;
    let Some(issue) = payload.issue else {
        return Ok(JiraEvent::Ignored(format!(
            "event `{}` carries no issue",
            payload.webhook_event
        )));
    };
    let external_ref = ExternalRef::new(name, &issue.key);
    let fields = issue.fields;

    match payload.webhook_event.as_str() {
        "jira:issue_created" => Ok(JiraEvent::Created(NewTicket {
            client: config.client.clone(),
            external_ref: Some(external_ref),
            title: fields.summary.unwrap_or_default(),
            description: fields
                .description
                .as_ref()
                .map(adf_to_text)
                .unwrap_or_default(),
            reporter: fields.reporter.and_then(User::label),
            labels: fields.labels.unwrap_or_default(),
            reported_severity: fields
                .priority
                .and_then(|p| severity_from_priority(&p.name)),
        })),
        "jira:issue_updated" if fields.resolution.is_some() => {
            let summary = config
                .resolution_field
                .as_ref()
                .and_then(|field| fields.extra.get(field))
                .and_then(field_text)
                .filter(|s| !s.trim().is_empty());
            Ok(JiraEvent::Resolved {
                external_ref,
                summary,
                resolved_by: fields.assignee.and_then(User::label),
            })
        }
        other => Ok(JiraEvent::Ignored(format!(
            "event `{other}` is not handled"
        ))),
    }
}

fn field_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => Some(adf_to_text(value)),
        _ => None,
    }
}

/// Checks a `sha256=<hex>` signature against the raw request body.
///
/// The comparison is constant-time.
#[must_use]
pub fn verify_signature(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex_digest) = header.trim().strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_digest) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use ticket_core::{ClientId, Severity};

    use super::*;

    fn config() -> JiraConfig {
        JiraConfig {
            client: ClientId::new("acme").unwrap(),
            base_url: "https://acme.atlassian.net".into(),
            email: "bot@acme.test".into(),
            api_token: "token".into(),
            webhook_secret: Some("s3cret".into()),
            resolve_transition_id: Some("31".into()),
            resolution_field: Some("customfield_10071".into()),
        }
    }

    #[test]
    fn issue_created_becomes_new_ticket() {
        let body = json!({
            "webhookEvent": "jira:issue_created",
            "issue": {
                "key": "OPS-42",
                "fields": {
                    "summary": "Checkout returns 500",
                    "description": { "type": "doc", "version": 1, "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "Since the 10:00 deploy" }] }
                    ]},
                    "priority": { "name": "Highest" },
                    "labels": ["checkout"],
                    "reporter": { "displayName": "Jane Doe" },
                    "resolution": null
                }
            }
        });
        let event = parse_webhook("jira-acme", &config(), body.to_string().as_bytes()).unwrap();
        let JiraEvent::Created(ticket) = event else {
            panic!("expected Created, got {event:?}");
        };
        assert_eq!(
            ticket.external_ref,
            Some(ExternalRef::new("jira-acme", "OPS-42"))
        );
        assert_eq!(ticket.title, "Checkout returns 500");
        assert_eq!(ticket.description, "Since the 10:00 deploy");
        assert_eq!(ticket.reported_severity, Some(Severity::Critical));
        assert_eq!(ticket.reporter.as_deref(), Some("Jane Doe"));
        assert_eq!(ticket.labels, vec!["checkout"]);
    }

    #[test]
    fn resolved_update_reads_configured_resolution_field() {
        let body = json!({
            "webhookEvent": "jira:issue_updated",
            "issue": {
                "key": "OPS-42",
                "fields": {
                    "summary": "Checkout returns 500",
                    "resolution": { "name": "Done" },
                    "assignee": { "displayName": "Sam", "emailAddress": "sam@acme.test" },
                    "customfield_10071": "Rolled back deploy 1.4.2"
                }
            }
        });
        let event = parse_webhook("jira-acme", &config(), body.to_string().as_bytes()).unwrap();
        assert_eq!(
            event,
            JiraEvent::Resolved {
                external_ref: ExternalRef::new("jira-acme", "OPS-42"),
                summary: Some("Rolled back deploy 1.4.2".into()),
                resolved_by: Some("sam@acme.test".into()),
            }
        );
    }

    #[test]
    fn unrelated_updates_are_ignored() {
        let body = json!({
            "webhookEvent": "jira:issue_updated",
            "issue": { "key": "OPS-1", "fields": { "summary": "x", "resolution": null } }
        });
        let event = parse_webhook("jira-acme", &config(), body.to_string().as_bytes()).unwrap();
        assert!(matches!(event, JiraEvent::Ignored(_)));
        assert!(parse_webhook("jira-acme", &config(), b"not json").is_err());
    }

    #[test]
    fn signature_verification() {
        let body = br#"{"webhookEvent":"jira:issue_created"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"s3cret").unwrap();
        mac.update(body);
        let header = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        assert!(verify_signature("s3cret", body, &header));
        assert!(!verify_signature("wrong", body, &header));
        assert!(!verify_signature("s3cret", b"tampered", &header));
        assert!(!verify_signature("s3cret", body, "sha1=abcd"));
        assert!(!verify_signature("s3cret", body, "sha256=zz"));
    }
}
