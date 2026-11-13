//! Parsing and authenticating Jira webhook deliveries.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;
use ticket_core::{
    CodeContext, Error, ExternalRef, Link, NewTicket, RepoId, Reporter, Result, TicketKind,
};

use crate::adf::adf_to_text;
use crate::config::JiraConfig;
use crate::priority::severity_from_priority;

/// Header carrying the webhook signature, as `sha256=<hex digest>`.
pub const SIGNATURE_HEADER: &str = "x-hub-signature";

/// What a webhook delivery means for the handler.
#[derive(Debug, Clone, PartialEq)]
pub enum JiraEvent {
    /// A new issue to triage.
    Created(Box<NewTicket>),
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
    issuetype: Option<Named>,
    components: Option<Vec<Named>>,
    created: Option<String>,
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
    account_id: Option<String>,
    display_name: Option<String>,
    email_address: Option<String>,
}

impl User {
    fn label(self) -> Option<String> {
        self.email_address.or(self.display_name)
    }

    /// `None` if Jira sent no usable identity (for example a privacy-hidden user).
    fn into_reporter(self) -> Option<Reporter> {
        let reporter = Reporter {
            name: self.display_name,
            email: self.email_address,
            external_id: self.account_id,
        };
        reporter.validate().ok().map(|()| reporter)
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
        "jira:issue_created" => Ok(JiraEvent::Created(Box::new(new_ticket(
            config,
            external_ref,
            fields,
        )))),
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

fn new_ticket(config: &JiraConfig, external_ref: ExternalRef, fields: Fields) -> NewTicket {
    let browse_url = format!(
        "{}/browse/{}",
        config.base_url.trim_end_matches('/'),
        external_ref.key
    );
    let mut metadata = BTreeMap::new();
    if let Some(issuetype) = &fields.issuetype {
        metadata.insert("jira.issuetype".to_owned(), issuetype.name.clone());
    }
    let repository = config
        .repository_field
        .as_ref()
        .and_then(|field| fields.extra.get(field))
        .and_then(option_text)
        .and_then(|value| RepoId::new(value).ok());

    NewTicket {
        client: config.client.clone(),
        external_ref: Some(external_ref),
        title: fields.summary.unwrap_or_default(),
        description: fields
            .description
            .as_ref()
            .map(adf_to_text)
            .unwrap_or_default(),
        reporter: fields.reporter.and_then(User::into_reporter),
        kind: fields
            .issuetype
            .map_or(TicketKind::Other, |t| kind_from_issue_type(&t.name)),
        labels: fields.labels.unwrap_or_default(),
        components: fields
            .components
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.name)
            .collect(),
        reported_severity: fields
            .priority
            .and_then(|p| severity_from_priority(&p.name)),
        code_context: repository.map(|repository| CodeContext {
            repository: Some(repository),
            ..CodeContext::default()
        }),
        links: vec![Link {
            url: browse_url,
            title: Some("Jira issue".into()),
        }],
        reported_at: fields.created.as_deref().and_then(parse_jira_time),
        metadata,
    }
}

/// Maps Jira's default issue type names (Software and Service Management) to a kind.
fn kind_from_issue_type(name: &str) -> TicketKind {
    match name.trim().to_ascii_lowercase().as_str() {
        "bug" | "defect" | "problem" => TicketKind::Bug,
        "incident" => TicketKind::Incident,
        "story" | "new feature" | "feature" | "improvement" | "epic" => TicketKind::FeatureRequest,
        "task" | "sub-task" | "subtask" | "change" | "service request" => TicketKind::Task,
        "question" | "support" => TicketKind::Question,
        _ => TicketKind::Other,
    }
}

/// Jira timestamps look like `2026-11-01T09:30:00.000+0000` (no colon in the offset).
fn parse_jira_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f%z")
        .or_else(|_| DateTime::parse_from_rfc3339(value))
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Text of a plain or single-select custom field.
fn option_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("value").and_then(Value::as_str).map(str::to_owned),
        _ => None,
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
            repository_field: Some("customfield_10080".into()),
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
                    "issuetype": { "name": "Bug" },
                    "components": [{ "id": "10001", "name": "checkout" }, { "name": "payments" }],
                    "created": "2026-11-01T09:30:00.000+0100",
                    "reporter": {
                        "accountId": "5b10ac8d82e05b22cc7d4ef5",
                        "displayName": "Jane Doe"
                    },
                    "customfield_10080": { "id": "10200", "value": "web-app" },
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
        let reporter = ticket.reporter.as_ref().unwrap();
        assert_eq!(reporter.name.as_deref(), Some("Jane Doe"));
        assert_eq!(
            reporter.external_id.as_deref(),
            Some("5b10ac8d82e05b22cc7d4ef5")
        );
        assert_eq!(reporter.email, None);
        assert_eq!(ticket.labels, vec!["checkout"]);
        assert_eq!(ticket.kind, TicketKind::Bug);
        assert_eq!(ticket.components, vec!["checkout", "payments"]);
        assert_eq!(
            ticket.reported_at.unwrap().to_rfc3339(),
            "2026-11-01T08:30:00+00:00"
        );
        assert_eq!(
            ticket
                .code_context
                .as_ref()
                .and_then(|c| c.repository.as_ref())
                .map(RepoId::as_str),
            Some("web-app")
        );
        assert_eq!(
            ticket.links[0].url,
            "https://acme.atlassian.net/browse/OPS-42"
        );
        assert_eq!(ticket.metadata["jira.issuetype"], "Bug");
        assert!(ticket.validate().is_ok());
    }

    #[test]
    fn minimal_issue_created_still_parses() {
        let body = json!({
            "webhookEvent": "jira:issue_created",
            "issue": { "key": "OPS-7", "fields": {
                "summary": "Question about invoices",
                "reporter": {},
                "customfield_10080": "not a valid id!",
                "created": "garbage"
            } }
        });
        let event = parse_webhook("jira-acme", &config(), body.to_string().as_bytes()).unwrap();
        let JiraEvent::Created(ticket) = event else {
            panic!("expected Created, got {event:?}");
        };
        assert_eq!(ticket.reporter, None);
        assert_eq!(ticket.kind, TicketKind::Other);
        assert_eq!(ticket.code_context, None);
        assert_eq!(ticket.reported_at, None);
        assert!(ticket.components.is_empty());
    }

    #[test]
    fn issue_types_map_to_kinds() {
        assert_eq!(kind_from_issue_type("Bug"), TicketKind::Bug);
        assert_eq!(kind_from_issue_type("Incident"), TicketKind::Incident);
        assert_eq!(kind_from_issue_type("Story"), TicketKind::FeatureRequest);
        assert_eq!(kind_from_issue_type("Sub-task"), TicketKind::Task);
        assert_eq!(kind_from_issue_type("Question"), TicketKind::Question);
        assert_eq!(kind_from_issue_type("Spike"), TicketKind::Other);
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
