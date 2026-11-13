use std::collections::HashMap;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use async_trait::async_trait;
use chrono::Utc;
use ticket_core::ports::{KnowledgeBase, TicketRepository};
use ticket_core::{
    Embedding, Error, ExternalRef, FixAttempt, KnowledgeEntry, Result, SimilarTicket, Ticket,
    TicketFilter, TicketId, TicketStatus, TriageReport, top_k,
};

/// A process-local store implementing both ports.
///
/// The lock is a `std` lock rather than a Tokio one because it is never held
/// across an `.await`.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    state: RwLock<State>,
}

#[derive(Debug, Default)]
struct State {
    tickets: HashMap<TicketId, Ticket>,
    triage: HashMap<TicketId, Vec<TriageReport>>,
    fixes: HashMap<TicketId, Vec<FixAttempt>>,
    knowledge: HashMap<TicketId, (KnowledgeEntry, Embedding)>,
}

impl InMemoryStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> Result<RwLockReadGuard<'_, State>> {
        self.state
            .read()
            .map_err(|_| Error::storage("in-memory store lock poisoned"))
    }

    fn write(&self) -> Result<RwLockWriteGuard<'_, State>> {
        self.state
            .write()
            .map_err(|_| Error::storage("in-memory store lock poisoned"))
    }
}

#[async_trait]
impl TicketRepository for InMemoryStore {
    async fn insert(&self, ticket: &Ticket) -> Result<()> {
        let mut state = self.write()?;
        if state.tickets.contains_key(&ticket.id) {
            return Err(Error::Conflict(format!(
                "ticket {} already exists",
                ticket.id
            )));
        }
        if let Some(external) = &ticket.external_ref
            && state
                .tickets
                .values()
                .any(|t| t.external_ref.as_ref() == Some(external))
        {
            return Err(Error::Conflict(format!("ticket {external} already exists")));
        }
        state.tickets.insert(ticket.id, ticket.clone());
        Ok(())
    }

    async fn get(&self, id: TicketId) -> Result<Option<Ticket>> {
        Ok(self.read()?.tickets.get(&id).cloned())
    }

    async fn find_by_external_ref(&self, external: &ExternalRef) -> Result<Option<Ticket>> {
        Ok(self
            .read()?
            .tickets
            .values()
            .find(|t| t.external_ref.as_ref() == Some(external))
            .cloned())
    }

    async fn set_status(&self, id: TicketId, status: TicketStatus) -> Result<()> {
        let mut state = self.write()?;
        let ticket = state
            .tickets
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("ticket {id}")))?;
        ticket.status = status;
        ticket.updated_at = Utc::now();
        Ok(())
    }

    async fn transition(
        &self,
        id: TicketId,
        from: &[TicketStatus],
        to: TicketStatus,
    ) -> Result<bool> {
        let mut state = self.write()?;
        match state.tickets.get_mut(&id) {
            Some(ticket) if from.contains(&ticket.status) => {
                ticket.status = to;
                ticket.updated_at = Utc::now();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn list_by_status(&self, status: TicketStatus, limit: u32) -> Result<Vec<Ticket>> {
        let state = self.read()?;
        let mut tickets: Vec<_> = state
            .tickets
            .values()
            .filter(|t| t.status == status)
            .cloned()
            .collect();
        tickets.sort_by_key(|t| (t.created_at, t.id));
        tickets.truncate(limit as usize);
        Ok(tickets)
    }

    async fn list(&self, filter: &TicketFilter) -> Result<Vec<Ticket>> {
        let state = self.read()?;
        let mut tickets: Vec<_> = state
            .tickets
            .values()
            .filter(|t| filter.status.is_none_or(|s| t.status == s))
            .filter(|t| filter.client.as_ref().is_none_or(|c| &t.client == c))
            .cloned()
            .collect();
        tickets.sort_by_key(|t| std::cmp::Reverse((t.created_at, t.id)));
        tickets.truncate(filter.limit as usize);
        Ok(tickets)
    }

    async fn save_triage(&self, report: &TriageReport) -> Result<()> {
        let mut state = self.write()?;
        if !state.tickets.contains_key(&report.ticket_id) {
            return Err(Error::NotFound(format!("ticket {}", report.ticket_id)));
        }
        state
            .triage
            .entry(report.ticket_id)
            .or_default()
            .push(report.clone());
        Ok(())
    }

    async fn latest_triage(&self, id: TicketId) -> Result<Option<TriageReport>> {
        Ok(self
            .read()?
            .triage
            .get(&id)
            .and_then(|reports| reports.last().cloned()))
    }

    async fn save_fix_attempt(&self, attempt: &FixAttempt) -> Result<()> {
        let mut state = self.write()?;
        if !state.tickets.contains_key(&attempt.ticket_id) {
            return Err(Error::NotFound(format!("ticket {}", attempt.ticket_id)));
        }
        state
            .fixes
            .entry(attempt.ticket_id)
            .or_default()
            .push(attempt.clone());
        Ok(())
    }

    async fn latest_fix_attempt(&self, id: TicketId) -> Result<Option<FixAttempt>> {
        Ok(self
            .read()?
            .fixes
            .get(&id)
            .and_then(|attempts| attempts.last().cloned()))
    }
}

#[async_trait]
impl KnowledgeBase for InMemoryStore {
    async fn upsert(&self, entry: &KnowledgeEntry, embedding: &Embedding) -> Result<()> {
        self.write()?
            .knowledge
            .insert(entry.ticket_id, (entry.clone(), embedding.clone()));
        Ok(())
    }

    async fn search(&self, query: &Embedding, limit: usize) -> Result<Vec<SimilarTicket>> {
        let state = self.read()?;
        let scored = state
            .knowledge
            .values()
            .map(|(entry, embedding)| (entry, embedding.cosine(query)));
        Ok(top_k(scored, limit)
            .into_iter()
            .map(|(entry, score)| SimilarTicket {
                entry: entry.clone(),
                score,
            })
            .collect())
    }

    async fn count(&self) -> Result<u64> {
        Ok(self.read()?.knowledge.len() as u64)
    }
}
