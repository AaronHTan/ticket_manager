use std::fmt;

use serde::Deserialize;
use ticket_core::ClientId;

/// Settings for one Jira Cloud site.
///
/// The connector *name* (the key under `[connectors.jira.<name>]` in
/// configuration) is passed separately. It becomes
/// [`ExternalRef::system`](ticket_core::ExternalRef::system) and the webhook
/// URL path segment.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JiraConfig {
    /// Client (tenant) that tickets from this site belong to.
    pub client: ClientId,
    /// Site URL, such as `https://acme.atlassian.net`.
    pub base_url: String,
    /// Account email used for API authentication.
    pub email: String,
    /// API token for that account.
    pub api_token: String,
    /// Shared secret configured on the Jira webhook. When set, unsigned or
    /// incorrectly signed deliveries are rejected.
    #[serde(default)]
    pub webhook_secret: Option<String>,
    /// Transition id used to resolve issues (`GET /rest/api/3/issue/{key}/transitions`).
    #[serde(default)]
    pub resolve_transition_id: Option<String>,
    /// Custom field holding the human-written fix, such as `customfield_10071`.
    /// Without it, resolutions in Jira close tickets but teach the system nothing.
    #[serde(default)]
    pub resolution_field: Option<String>,
    /// Field naming the affected repository, such as `customfield_10080`. Its
    /// value (text or a single-select option) must match a configured
    /// repository id.
    #[serde(default)]
    pub repository_field: Option<String>,
}

impl fmt::Debug for JiraConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JiraConfig")
            .field("client", &self.client)
            .field("base_url", &self.base_url)
            .field("email", &self.email)
            .field("api_token", &"<redacted>")
            .field(
                "webhook_secret",
                &self.webhook_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("resolve_transition_id", &self.resolve_transition_id)
            .field("resolution_field", &self.resolution_field)
            .field("repository_field", &self.repository_field)
            .finish()
    }
}
