//! Composition root: the only place that knows about concrete adapters.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use ticket_core::ports::{KnowledgeBase, TicketRepository};
use ticket_engine::{
    Dispatcher, LogNotifier, PipelineDeps, RuleBasedClassifier, TicketRouter, TriagePipeline,
    WorkerPool,
};
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

    let deps = PipelineDeps {
        tickets,
        knowledge,
        embedder,
        classifier,
    };
    let pipeline =
        Arc::new(TriagePipeline::new(deps, router, dispatcher).with_top_k(config.retrieval.top_k));

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

/// SQLite creates the file but not its directory.
fn ensure_sqlite_parent_dir(url: &str) -> anyhow::Result<()> {
    let path = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .map(|rest| rest.split('?').next().unwrap_or(rest));
    if let Some(parent) = path.and_then(|p| Path::new(p).parent())
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating database directory {}", parent.display()))?;
    }
    Ok(())
}
