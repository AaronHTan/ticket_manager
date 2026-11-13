//! Composition root: the only place that knows about concrete adapters.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use ticket_core::ports::{Fixer, KnowledgeBase, TicketRepository};
use ticket_engine::{
    Dispatcher, FixCoordinator, LogCodeHost, LogNotifier, NoopFixer, PatchFixer, PipelineDeps,
    RepositoryDirectory, RuleBasedClassifier, TicketRouter, TriagePipeline, WorkerPool,
};
use ticket_git::GitCli;
use ticket_jira::JiraClient;
use ticket_retrieval::HashingEmbedder;
use ticket_storage::{InMemoryStore, SqliteStore};
use tracing::{info, warn};

use crate::config::AppConfig;
use crate::state::AppState;

/// Maximum tickets re-queued at start-up.
const RECOVERY_LIMIT: u32 = 10_000;

/// A fully wired application.
pub struct App {
    /// Router state.
    pub state: AppState,
    /// Running triage workers. Join them after the server stops.
    pub workers: WorkerPool,
}

/// Builds every adapter from configuration, wires the pipeline and starts workers.
pub async fn build(config: &AppConfig) -> anyhow::Result<App> {
    let pipeline = build_pipeline(config).await?;

    let (queue, workers) = WorkerPool::spawn(
        Arc::clone(&pipeline),
        config.server.workers,
        config.server.queue_capacity,
    );

    let state = AppState {
        pipeline: Arc::clone(&pipeline),
        queue,
        jira: Arc::new(config.connectors.jira.clone()),
        api_token: config.server.api_token.as_deref().map(Arc::from),
    };

    let pending = pipeline.recover(RECOVERY_LIMIT).await?;
    if !pending.is_empty() {
        info!(
            count = pending.len(),
            "re-queueing tickets left from a previous run"
        );
    }
    for id in pending {
        state.enqueue(id);
    }

    Ok(App { state, workers })
}

/// Opens storage and wires the triage pipeline, including the fix path,
/// without starting workers, recovery or HTTP. Used by [`build`] and by
/// offline tools such as `ticket-seed`.
pub async fn build_pipeline(config: &AppConfig) -> anyhow::Result<Arc<TriagePipeline>> {
    let (tickets, knowledge) = open_storage(&config.database.url).await?;

    let embedder = Arc::new(HashingEmbedder::new(config.retrieval.embedding_dim)?);
    let classifier = Arc::new(match &config.classifier.rules {
        Some(rules) => RuleBasedClassifier::new(rules.clone()),
        None => RuleBasedClassifier::default(),
    });
    let router = TicketRouter::new(config.routing.clone(), config.escalation.clone())
        .context("invalid routing configuration")?;

    let mut dispatcher = Dispatcher::new(Arc::new(LogNotifier));
    for (name, jira) in &config.connectors.jira {
        if jira.webhook_secret.is_none() {
            warn!(connector = %name, "jira webhook_secret not set; webhooks are unauthenticated");
        }
        let client = JiraClient::new(name.clone(), jira.clone())
            .with_context(|| format!("jira connector `{name}`"))?;
        dispatcher = dispatcher.with_tracker(Arc::new(client));
    }
    info!(trackers = ?dispatcher.tracker_names().collect::<Vec<_>>(), "trackers registered");

    let fixes = build_fix_coordinator(config, &knowledge, &embedder).await?;

    let deps = PipelineDeps {
        tickets,
        knowledge,
        embedder,
        classifier,
    };
    Ok(Arc::new(
        TriagePipeline::new(deps, router, dispatcher)
            .with_top_k(config.retrieval.top_k)
            .with_fixes(Arc::new(fixes), config.fixes.clone()),
    ))
}

/// Wires the code-change path. Git is only checked when repositories are
/// configured, so the service still runs on hosts without git.
async fn build_fix_coordinator(
    config: &AppConfig,
    knowledge: &Arc<dyn KnowledgeBase>,
    embedder: &Arc<HashingEmbedder>,
) -> anyhow::Result<FixCoordinator> {
    config
        .fixes
        .validate()
        .context("invalid [fixes] configuration")?;
    let directory = RepositoryDirectory::new(config.repositories.clone())
        .context("invalid repository configuration")?;
    let git = Arc::new(GitCli::new(&config.git).context("invalid git configuration")?);
    if directory.is_empty() {
        if config.fixes.enabled {
            warn!("[fixes] is enabled but no [repositories] are configured; nothing can be fixed");
        }
    } else {
        let version = git.version().await.with_context(|| {
            format!(
                "repositories are configured but `{}` could not be run",
                config.git.binary.display()
            )
        })?;
        info!(
            %version,
            repositories = ?directory.ids().map(|id| id.as_str()).collect::<Vec<_>>(),
            fixes_enabled = config.fixes.enabled,
            "git ready"
        );
    }
    let fixer: Arc<dyn Fixer> = if config.fixes.enabled {
        Arc::new(PatchFixer::new(
            Arc::clone(knowledge),
            embedder.clone(),
            git.clone(),
            config.fixes.min_similarity,
        ))
    } else {
        Arc::new(NoopFixer)
    };
    Ok(FixCoordinator::new(
        directory,
        git,
        Arc::new(LogCodeHost),
        fixer,
        config.git.author(),
    ))
}

async fn open_storage(
    url: &str,
) -> anyhow::Result<(Arc<dyn TicketRepository>, Arc<dyn KnowledgeBase>)> {
    if url == "memory" {
        warn!("using in-memory storage; all data is lost on exit");
        let store = Arc::new(InMemoryStore::new());
        return Ok((store.clone(), store));
    }
    ensure_sqlite_parent_dir(url)?;
    let store = Arc::new(
        SqliteStore::connect(url)
            .await
            .with_context(|| format!("opening database `{url}`"))?,
    );
    info!(%url, "database ready");
    Ok((store.clone(), store))
}

/// The file path inside a SQLite URL such as `sqlite://data/app.db?mode=rwc`.
/// `None` for non-SQLite URLs and for in-memory databases.
#[must_use]
pub fn sqlite_path(url: &str) -> Option<&Path> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))?;
    let path = rest.split('?').next().unwrap_or(rest);
    (!path.is_empty() && path != ":memory:").then(|| Path::new(path))
}

/// SQLite creates the file but not its directory.
fn ensure_sqlite_parent_dir(url: &str) -> anyhow::Result<()> {
    if let Some(parent) = sqlite_path(url).and_then(Path::parent)
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating database directory {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_path_extracts_file_paths_only() {
        assert_eq!(
            sqlite_path("sqlite://data/test.db"),
            Some(Path::new("data/test.db"))
        );
        assert_eq!(
            sqlite_path("sqlite:/tmp/x.db?mode=rwc"),
            Some(Path::new("/tmp/x.db"))
        );
        assert_eq!(sqlite_path("sqlite::memory:"), None);
        assert_eq!(sqlite_path("memory"), None);
        assert_eq!(sqlite_path("postgres://db/x"), None);
    }
}
