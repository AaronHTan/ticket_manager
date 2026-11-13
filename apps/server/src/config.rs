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
            .merge(Env::prefixed(ENV_PREFIX).split("__"))
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
