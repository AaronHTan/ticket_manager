//! The error type shared across ports and adapters.

/// Type-erased error used to carry adapter-specific causes.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Convenient alias used throughout the workspace.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors that can cross a port boundary.
///
/// Variants describe *what went wrong* from the domain's point of view, so the
/// HTTP layer can map them to status codes without knowing which adapter
/// produced them.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The requested entity does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// The caller supplied invalid input.
    #[error("invalid input: {0}")]
    Validation(String),

    /// The operation conflicts with existing state (duplicate, concurrent claim, ...).
    #[error("conflict: {0}")]
    Conflict(String),

    /// The system is temporarily unable to accept work (queue full, shutting down).
    #[error("unavailable: {0}")]
    Unavailable(String),

    /// The persistence layer failed.
    #[error("storage error: {0}")]
    Storage(#[source] BoxError),

    /// An external system (Jira, an embedding API, ...) failed.
    #[error("upstream `{system}` error: {source}")]
    Upstream {
        /// Name of the external system.
        system: String,
        /// Underlying cause.
        #[source]
        source: BoxError,
    },

    /// The service was configured incorrectly.
    #[error("configuration error: {0}")]
    Config(String),
}

impl Error {
    /// Wraps a persistence-layer failure.
    pub fn storage(source: impl Into<BoxError>) -> Self {
        Self::Storage(source.into())
    }

    /// Wraps a failure from an external system.
    pub fn upstream(system: impl Into<String>, source: impl Into<BoxError>) -> Self {
        Self::Upstream {
            system: system.into(),
            source: source.into(),
        }
    }
}
