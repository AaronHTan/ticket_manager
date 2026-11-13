use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use ticket_core::ports::Tracker;
use ticket_core::{Assignee, Error, ExternalRef, Result, Severity};
use tracing::{debug, warn};

use crate::adf::text_to_adf;
use crate::config::JiraConfig;
use crate::priority::priority_for_severity;

/// A Jira Cloud REST v3 client implementing [`Tracker`].
#[derive(Debug, Clone)]
pub struct JiraClient {
    name: String,
    config: JiraConfig,
    base_url: String,
    http: reqwest::Client,
}

impl JiraClient {
    /// Request timeout for Jira API calls.
    pub const TIMEOUT: Duration = Duration::from_secs(15);

    /// Creates a client for connector `name`.
    pub fn new(name: impl Into<String>, config: JiraConfig) -> Result<Self> {
        let name = name.into();
        let base_url = config.base_url.trim_end_matches('/').to_owned();
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err(Error::Config(format!(
                "jira connector `{name}`: base_url must be an http(s) URL"
            )));
        }
        let http = reqwest::Client::builder()
            .timeout(Self::TIMEOUT)
            .user_agent(concat!("ticket-handler/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::upstream(&name, e))?;
        Ok(Self {
            name,
            config,
            base_url,
            http,
        })
    }

    /// The connector configuration.
    #[must_use]
    pub fn config(&self) -> &JiraConfig {
        &self.config
    }

    fn issue_url(&self, external: &ExternalRef, suffix: &str) -> Result<String> {
        if !is_valid_issue_key(&external.key) {
            return Err(Error::Validation(format!(
                "`{}` is not a valid Jira issue key",
                external.key
            )));
        }
        Ok(format!(
            "{}/rest/api/3/issue/{}{suffix}",
            self.base_url, external.key
        ))
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<()> {
        let response = request
            .basic_auth(&self.config.email, Some(&self.config.api_token))
            .send()
            .await
            .map_err(|e| Error::upstream(&self.name, e))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let mut body = response.text().await.unwrap_or_default();
        body.truncate(500);
        Err(Error::upstream(
            &self.name,
            format!("HTTP {status}: {body}"),
        ))
    }
}

/// Jira keys look like `PROJ-123`. Checking them keeps request paths well-formed.
fn is_valid_issue_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[async_trait]
impl Tracker for JiraClient {
    fn system(&self) -> &str {
        &self.name
    }

    async fn comment(&self, external: &ExternalRef, body: &str) -> Result<()> {
        let url = self.issue_url(external, "/comment")?;
        self.send(
            self.http
                .post(url)
                .json(&json!({ "body": text_to_adf(body) })),
        )
        .await
    }

    async fn assign(&self, external: &ExternalRef, assignee: &Assignee) -> Result<()> {
        let Some(account_id) = &assignee.external_id else {
            debug!(assignee = %assignee.name, "no Jira accountId configured; skipping assignment");
            return Ok(());
        };
        let url = self.issue_url(external, "/assignee")?;
        self.send(self.http.put(url).json(&json!({ "accountId": account_id })))
            .await
    }

    async fn set_severity(&self, external: &ExternalRef, severity: Severity) -> Result<()> {
        let url = self.issue_url(external, "")?;
        let body = json!({ "fields": { "priority": { "name": priority_for_severity(severity) } } });
        self.send(self.http.put(url).json(&body)).await
    }

    async fn mark_resolved(&self, external: &ExternalRef, _resolution: &str) -> Result<()> {
        let Some(transition_id) = &self.config.resolve_transition_id else {
            warn!(issue = %external, "resolve_transition_id not configured; leaving issue open");
            return Ok(());
        };
        let url = self.issue_url(external, "/transitions")?;
        self.send(
            self.http
                .post(url)
                .json(&json!({ "transition": { "id": transition_id } })),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use ticket_core::ClientId;

    use super::*;

    fn config(base_url: &str) -> JiraConfig {
        JiraConfig {
            client: ClientId::new("acme").unwrap(),
            base_url: base_url.into(),
            email: "bot@acme.test".into(),
            api_token: "token".into(),
            webhook_secret: None,
            resolve_transition_id: None,
            resolution_field: None,
            repository_field: None,
        }
    }

    #[test]
    fn builds_issue_urls_and_rejects_bad_keys() {
        let client = JiraClient::new("jira-acme", config("https://acme.atlassian.net/")).unwrap();
        let url = client
            .issue_url(&ExternalRef::new("jira-acme", "OPS-1"), "/comment")
            .unwrap();
        assert_eq!(
            url,
            "https://acme.atlassian.net/rest/api/3/issue/OPS-1/comment"
        );
        assert!(
            client
                .issue_url(&ExternalRef::new("jira-acme", "../../admin"), "")
                .is_err()
        );
    }

    #[test]
    fn rejects_non_http_base_url() {
        assert!(JiraClient::new("jira-acme", config("ftp://acme")).is_err());
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let rendered = format!("{:?}", config("https://acme.atlassian.net"));
        assert!(!rendered.contains("token\""));
        assert!(rendered.contains("<redacted>"));
    }
}
