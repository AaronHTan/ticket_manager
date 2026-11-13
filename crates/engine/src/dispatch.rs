//! Carrying out routing decisions against the outside world.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use ticket_core::ports::{Notifier, Tracker};
use ticket_core::{Assignee, FixResult, Result, RoutingDecision, Ticket, TriageReport};
use tracing::{info, warn};

/// Executes a [`TriageReport`]'s decision.
///
/// Trackers are keyed by connector name ([`ExternalRef::system`]). A ticket
/// from Jira instance `jira-acme` is updated through the tracker registered
/// under that name. Tickets submitted through the standalone API have no
/// external reference: their outcome is only recorded locally and exposed over
/// the API.
///
/// [`ExternalRef::system`]: ticket_core::ExternalRef::system
pub struct Dispatcher {
    trackers: HashMap<String, Arc<dyn Tracker>>,
    notifier: Arc<dyn Notifier>,
}

impl Dispatcher {
    /// Creates a dispatcher with no trackers.
    #[must_use]
    pub fn new(notifier: Arc<dyn Notifier>) -> Self {
        Self {
            trackers: HashMap::new(),
            notifier,
        }
    }

    /// Registers a tracker under its [`Tracker::system`] name.
    #[must_use]
    pub fn with_tracker(mut self, tracker: Arc<dyn Tracker>) -> Self {
        self.trackers.insert(tracker.system().to_owned(), tracker);
        self
    }

    /// Names of the registered trackers.
    pub fn tracker_names(&self) -> impl Iterator<Item = &str> {
        self.trackers.keys().map(String::as_str)
    }

    /// Applies the decision in `report` to `ticket`.
    pub async fn dispatch(&self, ticket: &Ticket, report: &TriageReport) -> Result<()> {
        if let Some(external) = &ticket.external_ref {
            match self.trackers.get(&external.system) {
                Some(tracker) => {
                    apply_to_tracker(tracker.as_ref(), ticket, report).await?;
                }
                None => warn!(
                    ticket = %ticket.id,
                    system = %external.system,
                    "no tracker registered for this system; outcome recorded locally only"
                ),
            }
        }
        if let Some(assignee) = report.decision.assignee() {
            self.notifier
                .notify(assignee, ticket, &report.decision)
                .await?;
        }
        Ok(())
    }
}

impl Dispatcher {
    /// Tells the external tracker, if any, about a proposed code change.
    /// Other fix results are only recorded locally.
    pub async fn report_fix(&self, ticket: &Ticket, result: &FixResult) -> Result<()> {
        let FixResult::Proposed {
            repo,
            branch,
            change_request,
            summary,
            ..
        } = result
        else {
            return Ok(());
        };
        let Some(external) = &ticket.external_ref else {
            return Ok(());
        };
        let Some(tracker) = self.trackers.get(&external.system) else {
            return Ok(());
        };
        let location = change_request
            .url
            .clone()
            .unwrap_or_else(|| format!("branch `{branch}` in `{repo}`"));
        tracker
            .comment(
                external,
                &format!("[triage] Proposed a code fix for review: {location}\n\n{summary}"),
            )
            .await
    }
}

async fn apply_to_tracker(
    tracker: &dyn Tracker,
    ticket: &Ticket,
    report: &TriageReport,
) -> Result<()> {
    let Some(external) = &ticket.external_ref else {
        return Ok(());
    };
    tracker
        .set_severity(external, report.classification.severity.severity)
        .await?;
    match &report.decision {
        RoutingDecision::AutoResolve { resolution, .. } => {
            tracker
                .comment(
                    external,
                    &format!(
                        "This matches a known issue and has been resolved automatically.\n\n{resolution}\n\n\
                         If this does not fix the problem, reply to reopen the ticket."
                    ),
                )
                .await?;
            tracker.mark_resolved(external, resolution).await?;
        }
        RoutingDecision::Forward {
            assignee,
            suggested_resolution,
            reason,
            score,
            ..
        } => {
            tracker.assign(external, assignee).await?;
            tracker
                .comment(
                    external,
                    &format!(
                        "[triage] Forwarded to {}: {reason}.\n\nSuggested fix (similarity {score:.2}):\n{suggested_resolution}",
                        assignee.name
                    ),
                )
                .await?;
        }
        RoutingDecision::Escalate { assignee, reason } => {
            tracker.assign(external, assignee).await?;
            let mut body = format!("[triage] Escalated to {}: {reason}.", assignee.name);
            if let Some(draft) = &report.draft {
                body.push_str("\n\n");
                body.push_str(&draft.text);
            }
            tracker.comment(external, &body).await?;
        }
    }
    Ok(())
}

/// A [`Notifier`] that writes to the log. Stands in for email or Slack.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogNotifier;

#[async_trait]
impl Notifier for LogNotifier {
    async fn notify(
        &self,
        assignee: &Assignee,
        ticket: &Ticket,
        decision: &RoutingDecision,
    ) -> Result<()> {
        info!(
            ticket = %ticket.id,
            assignee = %assignee.name,
            contact = assignee.contact.as_deref().unwrap_or("-"),
            status = %decision.resulting_status(),
            "notify assignee"
        );
        Ok(())
    }
}
