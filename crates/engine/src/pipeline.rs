//! Orchestration of ingest, triage, dispatch and learning.

use std::sync::Arc;

use chrono::Utc;
use ticket_core::ports::{Classifier, Embedder, KnowledgeBase, TicketRepository};
use ticket_core::{
    Category, Error, ExternalRef, HistoricalTicket, KnowledgeEntry, MatchSummary, NewTicket,
    Resolution, Result, Severity, Ticket, TicketId, TicketStatus, TriageReport,
};
use tracing::{info, instrument, warn};

use crate::dispatch::Dispatcher;
use crate::routing::TicketRouter;

/// The adapters the pipeline depends on, injected at start-up.
#[derive(Clone)]
pub struct PipelineDeps {
    /// Live ticket storage.
    pub tickets: Arc<dyn TicketRepository>,
    /// Resolved-ticket retrieval store.
    pub knowledge: Arc<dyn KnowledgeBase>,
    /// Text-to-vector model.
    pub embedder: Arc<dyn Embedder>,
    /// Severity and category classifier.
    pub classifier: Arc<dyn Classifier>,
}

/// Result of [`TriagePipeline::ingest`].
#[derive(Debug, Clone)]
pub struct Ingested {
    /// The stored ticket.
    pub ticket: Ticket,
    /// `false` if the ticket already existed (for example a redelivered webhook).
    pub created: bool,
}

/// The application service tying the ports together.
pub struct TriagePipeline {
    deps: PipelineDeps,
    router: TicketRouter,
    dispatcher: Dispatcher,
    top_k: usize,
}

impl TriagePipeline {
    /// Default number of knowledge-base hits considered per ticket.
    pub const DEFAULT_TOP_K: usize = 5;

    /// Creates a pipeline.
    #[must_use]
    pub fn new(deps: PipelineDeps, router: TicketRouter, dispatcher: Dispatcher) -> Self {
        Self {
            deps,
            router,
            dispatcher,
            top_k: Self::DEFAULT_TOP_K,
        }
    }

    /// Sets how many similar tickets to retrieve.
    #[must_use]
    pub fn with_top_k(mut self, top_k: usize) -> Self {
        self.top_k = top_k.max(1);
        self
    }

    /// Validates and stores an inbound ticket.
    ///
    /// Idempotent for tickets with an external reference: redelivering the
    /// same webhook returns the existing ticket instead of creating a duplicate.
    #[instrument(skip_all, fields(client = %new.client))]
    pub async fn ingest(&self, new: NewTicket) -> Result<Ingested> {
        if let Some(external) = &new.external_ref
            && let Some(existing) = self.deps.tickets.find_by_external_ref(external).await?
        {
            return Ok(Ingested {
                ticket: existing,
                created: false,
            });
        }

        let ticket = Ticket::receive(new, Utc::now())?;
        match self.deps.tickets.insert(&ticket).await {
            Ok(()) => {
                info!(ticket = %ticket.id, "ticket received");
                Ok(Ingested {
                    ticket,
                    created: true,
                })
            }
            // Lost a race with a concurrent delivery of the same external ticket.
            Err(Error::Conflict(_)) if ticket.external_ref.is_some() => {
                let external = ticket.external_ref.as_ref().expect("checked above");
                let existing = self
                    .deps
                    .tickets
                    .find_by_external_ref(external)
                    .await?
                    .ok_or_else(|| Error::Conflict(format!("ticket {external} vanished")))?;
                Ok(Ingested {
                    ticket: existing,
                    created: false,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Claims, triages and dispatches one ticket.
    ///
    /// Safe to call more than once for the same id: only one caller wins the
    /// claim, and the others get the existing report or a conflict.
    #[instrument(skip(self))]
    pub async fn process(&self, id: TicketId) -> Result<TriageReport> {
        let tickets = &self.deps.tickets;
        if tickets.get(id).await?.is_none() {
            return Err(Error::NotFound(format!("ticket {id}")));
        }
        let claimed = tickets
            .transition(
                id,
                &[TicketStatus::Received, TicketStatus::Failed],
                TicketStatus::Triaging,
            )
            .await?;
        if !claimed {
            return tickets
                .latest_triage(id)
                .await?
                .ok_or_else(|| Error::Conflict(format!("ticket {id} is already being triaged")));
        }

        // Re-read after claiming so we act on the latest state.
        let ticket = tickets
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("ticket {id}")))?;

        let outcome = async {
            let report = self.triage(&ticket).await?;
            tickets.save_triage(&report).await?;
            self.dispatcher.dispatch(&ticket, &report).await?;
            Ok::<_, Error>(report)
        }
        .await;

        match outcome {
            Ok(report) => {
                let status = report.decision.resulting_status();
                tickets.set_status(id, status).await?;
                info!(%status, severity = %report.classification.severity.severity, "ticket triaged");
                Ok(report)
            }
            Err(error) => {
                warn!(%error, "triage failed");
                tickets.set_status(id, TicketStatus::Failed).await?;
                Err(error)
            }
        }
    }

    /// Runs retrieval, classification and routing with no side effects.
    pub async fn triage(&self, ticket: &Ticket) -> Result<TriageReport> {
        let embedding = self.deps.embedder.embed(&ticket.searchable_text()).await?;
        let matches = self.deps.knowledge.search(&embedding, self.top_k).await?;
        let classification = self.deps.classifier.classify(ticket, &matches).await?;
        let decision = self.router.decide(&classification, &matches);
        Ok(TriageReport {
            ticket_id: ticket.id,
            classification,
            matches: matches.iter().map(MatchSummary::from).collect(),
            decision,
            decided_at: Utc::now(),
        })
    }

    /// Records how a human resolved a ticket and adds it to the knowledge base.
    ///
    /// This is the feedback loop that lets the system handle more tickets on
    /// its own over time. `category` and `severity` override the triage-time
    /// values when the human corrected them.
    pub async fn record_resolution(
        &self,
        id: TicketId,
        resolution: Resolution,
        category: Option<Category>,
        severity: Option<Severity>,
    ) -> Result<KnowledgeEntry> {
        if resolution.summary.trim().is_empty() {
            return Err(Error::Validation(
                "resolution summary must not be empty".into(),
            ));
        }
        let ticket = self
            .deps
            .tickets
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("ticket {id}")))?;
        let triage = self.deps.tickets.latest_triage(id).await?;

        let entry = KnowledgeEntry {
            ticket_id: ticket.id,
            title: ticket.title,
            description: ticket.description,
            category: category.or_else(|| {
                triage
                    .as_ref()
                    .and_then(|t| t.classification.category.clone())
            }),
            severity: severity
                .or_else(|| triage.as_ref().map(|t| t.classification.severity.severity))
                .unwrap_or(Severity::Medium),
            resolution,
            updated_at: Utc::now(),
        };
        self.index(&entry).await?;
        self.deps
            .tickets
            .set_status(id, TicketStatus::Resolved)
            .await?;
        info!(ticket = %id, automatable = entry.resolution.automatable, "resolution learned");
        Ok(entry)
    }

    /// Handles a resolution reported by an external tracker, such as a Jira
    /// issue moving to Done.
    ///
    /// Echoes of our own auto-resolutions are ignored, so the system never
    /// learns from itself. Externally learned fixes are never marked
    /// automatable: a human has to opt in through [`Self::record_resolution`].
    pub async fn record_external_resolution(
        &self,
        external: &ExternalRef,
        summary: Option<String>,
        resolved_by: Option<String>,
    ) -> Result<Option<KnowledgeEntry>> {
        let Some(ticket) = self.deps.tickets.find_by_external_ref(external).await? else {
            return Ok(None);
        };
        match ticket.status {
            TicketStatus::AutoResolved | TicketStatus::Resolved => Ok(None),
            _ => match summary.filter(|s| !s.trim().is_empty()) {
                Some(summary) => {
                    let resolution = Resolution {
                        summary,
                        automatable: false,
                        resolved_by,
                    };
                    self.record_resolution(ticket.id, resolution, None, None)
                        .await
                        .map(Some)
                }
                None => {
                    self.deps
                        .tickets
                        .set_status(ticket.id, TicketStatus::Resolved)
                        .await?;
                    Ok(None)
                }
            },
        }
    }

    /// Seeds the knowledge base with previously resolved tickets.
    ///
    /// Every entry is validated before any is written, so a bad batch is rejected whole.
    pub async fn import_history(&self, history: Vec<HistoricalTicket>) -> Result<usize> {
        for item in &history {
            item.validate()?;
        }
        let now = Utc::now();
        let count = history.len();
        for item in history {
            self.index(&item.into_entry(now)).await?;
        }
        info!(count, "imported historical tickets");
        Ok(count)
    }

    /// Prepares for work after a restart.
    ///
    /// Tickets stuck in `triaging` (the process died mid-triage) are reset to
    /// `received`, and the ids of all `received` tickets are returned for
    /// re-queueing. This assumes a single instance; with several replicas,
    /// stale claims should instead expire by `updated_at`.
    pub async fn recover(&self, limit: u32) -> Result<Vec<TicketId>> {
        let tickets = &self.deps.tickets;
        for ticket in tickets
            .list_by_status(TicketStatus::Triaging, limit)
            .await?
        {
            tickets
                .transition(ticket.id, &[TicketStatus::Triaging], TicketStatus::Received)
                .await?;
        }
        Ok(tickets
            .list_by_status(TicketStatus::Received, limit)
            .await?
            .into_iter()
            .map(|t| t.id)
            .collect())
    }

    /// Fetches a ticket and its latest triage report.
    pub async fn ticket_with_triage(
        &self,
        id: TicketId,
    ) -> Result<Option<(Ticket, Option<TriageReport>)>> {
        let Some(ticket) = self.deps.tickets.get(id).await? else {
            return Ok(None);
        };
        let triage = self.deps.tickets.latest_triage(id).await?;
        Ok(Some((ticket, triage)))
    }

    /// Number of knowledge-base entries.
    pub async fn knowledge_size(&self) -> Result<u64> {
        self.deps.knowledge.count().await
    }

    async fn index(&self, entry: &KnowledgeEntry) -> Result<()> {
        let embedding = self.deps.embedder.embed(&entry.searchable_text()).await?;
        self.deps.knowledge.upsert(entry, &embedding).await
    }
}
