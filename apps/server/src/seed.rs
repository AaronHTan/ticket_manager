//! Loading fixture data into a database, for local runs and tests.
//!
//! Fixtures are plain JSON in the same shapes the HTTP API accepts:
//!
//! * a history file: an array of [`HistoricalTicket`], indexed into the knowledge base;
//! * a tickets file: an array of [`FixtureTicket`], each a
//!   [`SubmitTicketRequest`] plus the status triage is expected to reach.
//!
//! Tickets are ingested and triaged inline, in file order, through the real
//! [`TriagePipeline`]. Because the embedder and classifier are
//! deterministic, the same fixtures always produce the same database, and
//! the expected statuses double as a regression check on routing.

use std::path::Path;

use anyhow::{Context, bail};
use serde::Deserialize;
use ticket_api::{HistoricalTicket, NewTicket, SubmitTicketRequest, TicketId, TicketStatus};
use ticket_engine::TriagePipeline;

/// One incoming ticket and the status triage should leave it in.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureTicket {
    /// The ticket, exactly as it would be submitted to `POST /v1/tickets`.
    /// It must have an `external_ref`, which makes re-seeding idempotent.
    pub ticket: SubmitTicketRequest,
    /// `auto_resolved`, `forwarded` or `escalated`, or `received` to store the
    /// ticket without triaging it (it is then picked up by server recovery).
    pub expect: TicketStatus,
}

/// What happened to one fixture ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeededTicket {
    /// External key, such as `FX-3`.
    pub key: String,
    /// Ticket title.
    pub title: String,
    /// Stored id.
    pub id: TicketId,
    /// `false` if the ticket was already in the database.
    pub created: bool,
    /// Status the fixture expects.
    pub expected: TicketStatus,
    /// Status the ticket ended in.
    pub actual: TicketStatus,
}

impl SeededTicket {
    /// True if triage reached the expected status.
    #[must_use]
    pub fn matches(&self) -> bool {
        self.expected == self.actual
    }
}

/// Outcome of [`seed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedReport {
    /// History entries indexed. `None` if the knowledge base already had
    /// entries, in which case history was not imported again.
    pub imported: Option<usize>,
    /// Knowledge-base size afterwards.
    pub knowledge_entries: u64,
    /// One entry per fixture ticket, in file order.
    pub tickets: Vec<SeededTicket>,
}

impl SeedReport {
    /// Tickets that did not reach their expected status.
    pub fn mismatches(&self) -> impl Iterator<Item = &SeededTicket> {
        self.tickets.iter().filter(|t| !t.matches())
    }
}

/// Reads a history file.
pub fn load_history(path: &Path) -> anyhow::Result<Vec<HistoricalTicket>> {
    read_json(path)
}

/// Reads a tickets file and checks every entry can be seeded idempotently.
pub fn load_tickets(path: &Path) -> anyhow::Result<Vec<FixtureTicket>> {
    let tickets: Vec<FixtureTicket> = read_json(path)?;
    for (i, fixture) in tickets.iter().enumerate() {
        if fixture.ticket.external_ref.is_none() {
            bail!(
                "{}: ticket #{} (`{}`) needs an external_ref so re-seeding does not duplicate it",
                path.display(),
                i + 1,
                fixture.ticket.title
            );
        }
        if matches!(
            fixture.expect,
            TicketStatus::Triaging | TicketStatus::Resolved | TicketStatus::Failed
        ) {
            bail!(
                "{}: ticket #{} expects `{}`, which triage never produces",
                path.display(),
                i + 1,
                fixture.expect
            );
        }
    }
    Ok(tickets)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

/// Imports `history` (only into an empty knowledge base), then ingests each
/// ticket and, unless it expects `received`, triages it.
///
/// Safe to run repeatedly against the same database: history is not
/// re-imported and tickets are deduplicated by external reference.
pub async fn seed(
    pipeline: &TriagePipeline,
    history: Vec<HistoricalTicket>,
    tickets: Vec<FixtureTicket>,
) -> anyhow::Result<SeedReport> {
    let imported = if pipeline.knowledge_size().await? == 0 {
        Some(
            pipeline
                .import_history(history)
                .await
                .context("importing history")?,
        )
    } else {
        None
    };

    let mut seeded = Vec::with_capacity(tickets.len());
    for FixtureTicket { ticket, expect } in tickets {
        let key = ticket
            .external_ref
            .as_ref()
            .map(|r| r.key.clone())
            .unwrap_or_default();
        let ingested = pipeline
            .ingest(NewTicket::from(ticket))
            .await
            .with_context(|| format!("ingesting fixture {key}"))?;
        let id = ingested.ticket.id;
        if expect != TicketStatus::Received && ingested.ticket.status == TicketStatus::Received {
            pipeline
                .process(id)
                .await
                .with_context(|| format!("triaging fixture {key}"))?;
        }
        let (ticket, _) = pipeline
            .ticket_with_triage(id)
            .await?
            .with_context(|| format!("fixture {key} disappeared after ingest"))?;
        seeded.push(SeededTicket {
            key,
            title: ticket.title,
            id,
            created: ingested.created,
            expected: expect,
            actual: ticket.status,
        });
    }

    Ok(SeedReport {
        imported,
        knowledge_entries: pipeline.knowledge_size().await?,
        tickets: seeded,
    })
}
