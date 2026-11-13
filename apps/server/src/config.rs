//! Layered configuration: a TOML file, then `TICKET_HANDLER_*` environment variables.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::Deserialize;
use ticket_core::RepoId;
use ticket_engine::{EscalationDirectory, KeywordRule, RepositoryConfig, RoutingPolicy};
use ticket_git::GitConfig;
use ticket_jira::JiraConfig;

/// Environment variable prefix. `__` separates nesting levels, for example
/// `TICKET_HANDLER_SERVER__BIND=0.0.0.0:9000`.
pub const ENV_PREFIX: &str = "TICKET_HANDLER_";

/// Variables that share [`ENV_PREFIX`] but are not configuration keys:
/// `TICKET_HANDLER_CONFIG` selects the file, and `TICKET_HANDLER_URL` and
/// `TICKET_HANDLER_TOKEN` belong to the `ticket` CLI. Without this list they
/// would be read as unknown top-level keys and fail start-up.
const NON_CONFIG_VARS: &[&str] = &["config", "url", "token"];

/// Root configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// HTTP server and worker settings.
    #[serde(default)]
    pub server: ServerConfig,
    /// Persistence settings.
    pub database: DatabaseConfig,
    /// Embedding and retrieval settings.
    #[serde(default)]
    pub retrieval: RetrievalConfig,
    /// Routing thresholds.
    #[serde(default)]
    pub routing: RoutingPolicy,
    /// Who receives forwarded and escalated tickets.
    pub escalation: EscalationDirectory,
    /// Classifier settings.
    #[serde(default)]
    pub classifier: ClassifierConfig,
    /// External system connectors.
    #[serde(default)]
    pub connectors: ConnectorsConfig,
    /// How the service runs git.
    #[serde(default)]
    pub git: GitConfig,
    /// Repositories the service may change, keyed by repository id.
    #[serde(default)]
    pub repositories: BTreeMap<RepoId, RepositoryConfig>,
}

impl AppConfig {
    /// Loads `path`, then applies environment overrides.
    pub fn load(path: &Path) -> Result<Self, Box<figment::Error>> {
        Figment::new()
            .merge(Toml::file_exact(path))
            .merge(
                Env::prefixed(ENV_PREFIX)
                    .ignore(NON_CONFIG_VARS)
                    .split("__"),
            )
            .extract()
            .map_err(Box::new)
    }

    /// Parses configuration from a TOML string, with no environment overrides. For tests.
    pub fn from_toml(toml: &str) -> Result<Self, Box<figment::Error>> {
        Figment::from(Toml::string(toml))
            .extract()
            .map_err(Box::new)
    }
}

/// HTTP server and worker settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Listen address.
    pub bind: SocketAddr,
    /// Number of triage workers.
    pub workers: usize,
    /// Maximum queued tickets before submissions are deferred.
    pub queue_capacity: usize,
    /// When set, `/v1` endpoints (other than webhooks) require `Authorization: Bearer <token>`.
    pub api_token: Option<String>,
    /// Log output format.
    pub log_format: LogFormat,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 8080)),
            workers: 4,
            queue_capacity: 1024,
            api_token: None,
            log_format: LogFormat::Pretty,
        }
    }
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// Human-readable, for local development.
    Pretty,
    /// One JSON object per line, for log aggregation.
    Json,
}

/// Persistence settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// `memory` for an ephemeral in-process store, or a SQLite URL such as
    /// `sqlite://data/ticket_handler.db`.
    pub url: String,
}

/// Embedding and retrieval settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetrievalConfig {
    /// Hashing-embedder vector width.
    pub embedding_dim: usize,
    /// Number of similar tickets retrieved per triage.
    pub top_k: usize,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            embedding_dim: 512,
            top_k: 5,
        }
    }
}

/// Classifier settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClassifierConfig {
    /// Replaces the built-in keyword rules when set.
    pub rules: Option<Vec<KeywordRule>>,
}

/// External system connectors, keyed by connector name.
///
/// A map rather than a list so each instance can be targeted by environment
/// variables, such as `TICKET_HANDLER_CONNECTORS__JIRA__ACME__API_TOKEN`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectorsConfig {
    /// Jira Cloud sites.
    pub jira: BTreeMap<String, JiraConfig>,
}

#[cfg(test)]
mod tests {
    use figment::Jail;

    use super::*;

    #[test]
    #[expect(
        clippy::result_large_err,
        reason = "figment::Jail fixes the closure's error type"
    )]
    fn env_overrides_apply_and_non_config_variables_are_ignored() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "app.toml",
                r#"
                [database]
                url = "sqlite://data/app.db"
                [escalation.intermediate]
                name = "L2"
                [escalation.default_expert]
                name = "On-call"
                "#,
            )?;
            jail.set_env("TICKET_HANDLER_CONFIG", "app.toml");
            jail.set_env("TICKET_HANDLER_URL", "http://127.0.0.1:8080");
            jail.set_env("TICKET_HANDLER_TOKEN", "secret");
            jail.set_env("TICKET_HANDLER_DATABASE__URL", "sqlite://data/test.db");

            let config = AppConfig::load(Path::new("app.toml")).map_err(|e| e.to_string())?;
            assert_eq!(config.database.url, "sqlite://data/test.db");
            assert_eq!(config.server.api_token, None);

            // A genuinely unknown key still fails loudly.
            jail.set_env("TICKET_HANDLER_BOGUS", "1");
            assert!(AppConfig::load(Path::new("app.toml")).is_err());
            Ok(())
        });
    }
}
