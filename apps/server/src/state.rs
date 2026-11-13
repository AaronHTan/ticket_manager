use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ticket_core::TicketId;
use ticket_engine::{TriagePipeline, TriageQueue};
use ticket_jira::JiraConfig;
use tracing::warn;

/// Shared state handed to every request handler. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    /// The triage pipeline.
    pub pipeline: Arc<TriagePipeline>,
    /// Queue feeding the triage workers.
    pub queue: TriageQueue,
    /// Jira connectors by name, for webhook authentication and parsing.
    pub jira: Arc<BTreeMap<String, JiraConfig>>,
    /// Bearer token protecting the `/v1` API, if configured.
    pub api_token: Option<Arc<str>>,
    /// Whether new tickets are triaged (and fixed) automatically. Shared so
    /// `PUT /v1/settings` can flip it when `mode_switchable` is set.
    pub auto_triage: Arc<AtomicBool>,
    /// Whether the mode may be changed at runtime (`server.allow_mode_switch`).
    pub mode_switchable: bool,
}

impl AppState {
    /// Whether new tickets are triaged automatically right now.
    #[must_use]
    pub fn auto_triage(&self) -> bool {
        self.auto_triage.load(Ordering::Relaxed)
    }

    /// Switches between automatic and step-by-step triage.
    pub fn set_auto_triage(&self, enabled: bool) {
        self.auto_triage.store(enabled, Ordering::Relaxed);
    }

    /// Queues a ticket for triage, deferring it if the queue is saturated.
    ///
    /// The ticket is already persisted as `received`, so a deferred ticket is
    /// not lost. It is picked up by [`TriagePipeline::recover`] on restart.
    ///
    /// Does nothing when automatic triage is off: the ticket then waits for
    /// an explicit `POST /v1/tickets/{id}/triage`.
    pub fn enqueue(&self, id: TicketId) {
        if !self.auto_triage() {
            return;
        }
        if let Err(error) = self.queue.try_enqueue(id) {
            warn!(ticket = %id, %error, "could not queue ticket; it remains `received`");
        }
    }
}
