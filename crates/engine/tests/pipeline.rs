//! End-to-end pipeline tests using real adapters (in-memory store, hashing
//! embedder, rule-based classifier). The only test double is a tracker that
//! records the calls it would have made to an external system.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ticket_core::ports::{TicketRepository, Tracker};
use ticket_core::{
    Assignee, Category, ClientId, ExternalRef, HistoricalTicket, NewTicket, Reporter, Resolution,
    Result, RoutingDecision, Severity, TicketStatus,
};
use ticket_engine::{
    Dispatcher, EscalationDirectory, LogNotifier, PipelineDeps, RoutingPolicy, RuleBasedClassifier,
    TicketRouter, TriagePipeline, WorkerPool,
};
use ticket_retrieval::HashingEmbedder;
use ticket_storage::InMemoryStore;

#[derive(Default)]
struct RecordingTracker {
    calls: Mutex<Vec<String>>,
}

impl RecordingTracker {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

#[async_trait]
impl Tracker for RecordingTracker {
    fn system(&self) -> &str {
        "jira-acme"
    }
    async fn comment(&self, external: &ExternalRef, _body: &str) -> Result<()> {
        self.record(format!("comment {}", external.key));
        Ok(())
    }
    async fn assign(&self, external: &ExternalRef, assignee: &Assignee) -> Result<()> {
        self.record(format!("assign {} {}", external.key, assignee.name));
        Ok(())
    }
    async fn set_severity(&self, external: &ExternalRef, severity: Severity) -> Result<()> {
        self.record(format!("severity {} {severity}", external.key));
        Ok(())
    }
    async fn mark_resolved(&self, external: &ExternalRef, _resolution: &str) -> Result<()> {
        self.record(format!("resolve {}", external.key));
        Ok(())
    }
}

fn assignee(name: &str) -> Assignee {
    Assignee {
        name: name.into(),
        contact: Some(format!("{}@acme.test", name.to_lowercase())),
        external_id: None,
    }
}

struct Harness {
    pipeline: Arc<TriagePipeline>,
    store: Arc<InMemoryStore>,
    tracker: Arc<RecordingTracker>,
}

async fn harness() -> Harness {
    let store = Arc::new(InMemoryStore::new());
    let tracker = Arc::new(RecordingTracker::default());
    let deps = PipelineDeps {
        tickets: store.clone(),
        knowledge: store.clone(),
        embedder: Arc::new(HashingEmbedder::default()),
        classifier: Arc::new(RuleBasedClassifier::default()),
    };
    let directory = EscalationDirectory {
        intermediate: assignee("L2"),
        default_expert: assignee("Platform"),
        experts: HashMap::from([(Category::new("security").unwrap(), assignee("Security"))]),
    };
    let router = TicketRouter::new(RoutingPolicy::default(), directory).unwrap();
    let dispatcher = Dispatcher::new(Arc::new(LogNotifier)).with_tracker(tracker.clone());
    let pipeline = Arc::new(TriagePipeline::new(deps, router, dispatcher));

    pipeline
        .import_history(vec![
            HistoricalTicket {
                title: "Password reset email not received".into(),
                description: "User requested a password reset but the reset email never arrived. Spam folder checked.".into(),
                category: Some(Category::new("authentication").unwrap()),
                severity: Severity::Low,
                resolution: Resolution {
                    summary: "Re-send the reset link from the admin console and allow-list noreply@acme.test.".into(),
                    automatable: true,
                    resolved_by: Some("l2".into()),
                    patch: None,
                },
            },
            HistoricalTicket {
                title: "Invoice shows duplicate charge".into(),
                description: "Customer was charged twice for the monthly subscription invoice.".into(),
                category: Some(Category::new("billing").unwrap()),
                severity: Severity::Medium,
                resolution: Resolution {
                    summary: "Void the duplicate invoice in the billing system and issue a refund.".into(),
                    automatable: false,
                    resolved_by: Some("billing".into()),
                    patch: None,
                },
            },
        ])
        .await
        .unwrap();

    Harness {
        pipeline,
        store,
        tracker,
    }
}

fn new_ticket(title: &str, description: &str, jira_key: Option<&str>) -> NewTicket {
    let new = NewTicket::new(ClientId::new("acme").unwrap(), title, description)
        .with_reporter(Reporter::from_label("jane@acme.test").unwrap());
    match jira_key {
        Some(key) => new.with_external_ref(ExternalRef::new("jira-acme", key)),
        None => new,
    }
}

#[tokio::test]
async fn known_issue_with_automatable_fix_is_auto_resolved_in_jira() {
    let h = harness().await;
    let ingested = h
        .pipeline
        .ingest(new_ticket(
            "Password reset email not received",
            "User requested a password reset but the reset email never arrived. Spam folder checked.",
            Some("OPS-1"),
        ))
        .await
        .unwrap();

    let report = h.pipeline.process(ingested.ticket.id).await.unwrap();

    assert!(
        matches!(report.decision, RoutingDecision::AutoResolve { score, .. } if score > 0.99),
        "{:?}",
        report.decision
    );
    assert_eq!(
        report.classification.category.unwrap().as_str(),
        "authentication"
    );
    assert_eq!(
        h.tracker.calls(),
        vec!["severity OPS-1 low", "comment OPS-1", "resolve OPS-1"]
    );
    let stored = h.store.get(ingested.ticket.id).await.unwrap().unwrap();
    assert_eq!(stored.status, TicketStatus::AutoResolved);
}

#[tokio::test]
async fn known_issue_without_automatable_fix_is_forwarded() {
    let h = harness().await;
    let ingested = h
        .pipeline
        .ingest(new_ticket(
            "Invoice shows duplicate charge",
            "Customer was charged twice for the monthly subscription invoice.",
            Some("OPS-2"),
        ))
        .await
        .unwrap();

    let report = h.pipeline.process(ingested.ticket.id).await.unwrap();

    match &report.decision {
        RoutingDecision::Forward {
            assignee,
            suggested_resolution,
            ..
        } => {
            assert_eq!(assignee.name, "L2");
            assert!(suggested_resolution.contains("refund"));
        }
        other => panic!("expected forward, got {other:?}"),
    }
    assert_eq!(
        h.tracker.calls(),
        vec!["severity OPS-2 medium", "assign OPS-2 L2", "comment OPS-2"]
    );
}

#[tokio::test]
async fn novel_security_issue_is_escalated_to_security_expert() {
    let h = harness().await;
    let ingested = h
        .pipeline
        .ingest(new_ticket(
            "Suspicious admin logins from unknown country",
            "We think there was unauthorized access to the admin panel overnight.",
            None,
        ))
        .await
        .unwrap();

    let report = h.pipeline.process(ingested.ticket.id).await.unwrap();

    assert_eq!(report.classification.severity.severity, Severity::Critical);
    assert!(
        matches!(&report.decision, RoutingDecision::Escalate { assignee, .. } if assignee.name == "Security"),
        "{:?}",
        report.decision
    );
    // Standalone ticket: nothing is sent to any tracker.
    assert!(h.tracker.calls().is_empty());
}

#[tokio::test]
async fn redelivered_webhook_does_not_duplicate_ticket() {
    let h = harness().await;
    let first = h
        .pipeline
        .ingest(new_ticket("Printer jam", "Floor 3", Some("OPS-9")))
        .await
        .unwrap();
    let second = h
        .pipeline
        .ingest(new_ticket("Printer jam", "Floor 3", Some("OPS-9")))
        .await
        .unwrap();
    assert!(first.created);
    assert!(!second.created);
    assert_eq!(first.ticket.id, second.ticket.id);
}

#[tokio::test]
async fn processing_twice_returns_existing_report_without_redispatching() {
    let h = harness().await;
    let id = h
        .pipeline
        .ingest(new_ticket("Printer jam", "Floor 3", Some("OPS-10")))
        .await
        .unwrap()
        .ticket
        .id;
    let first = h.pipeline.process(id).await.unwrap();
    let calls_after_first = h.tracker.calls().len();
    let second = h.pipeline.process(id).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(h.tracker.calls().len(), calls_after_first);
}

#[tokio::test]
async fn learned_resolution_makes_the_next_similar_ticket_known() {
    let h = harness().await;
    let novel = new_ticket(
        "VPN client disconnects every ten minutes",
        "The corporate VPN client drops the tunnel every ten minutes on macOS.",
        None,
    );
    let first = h.pipeline.ingest(novel.clone()).await.unwrap().ticket.id;
    let report = h.pipeline.process(first).await.unwrap();
    assert!(matches!(report.decision, RoutingDecision::Escalate { .. }));

    // An expert fixes it and confirms the fix is safe to automate.
    h.pipeline
        .record_resolution(
            first,
            Resolution {
                summary: "Upgrade the VPN client to 5.2 which fixes the keep-alive bug.".into(),
                automatable: true,
                resolved_by: Some("network-team".into()),
                patch: None,
            },
            Some(Category::new("network").unwrap()),
            Some(Severity::Low),
        )
        .await
        .unwrap();
    assert_eq!(
        h.store.get(first).await.unwrap().unwrap().status,
        TicketStatus::Resolved
    );

    // The same problem reported again is now handled automatically.
    let second = h.pipeline.ingest(novel).await.unwrap().ticket.id;
    let report = h.pipeline.process(second).await.unwrap();
    assert!(
        matches!(report.decision, RoutingDecision::AutoResolve { matched, .. } if matched == first),
        "{:?}",
        report.decision
    );
}

#[tokio::test]
async fn external_resolution_of_auto_resolved_ticket_is_ignored() {
    let h = harness().await;
    let id = h
        .pipeline
        .ingest(new_ticket(
            "Password reset email not received",
            "User requested a password reset but the reset email never arrived. Spam folder checked.",
            Some("OPS-3"),
        ))
        .await
        .unwrap()
        .ticket
        .id;
    h.pipeline.process(id).await.unwrap();
    let before = h.pipeline.knowledge_size().await.unwrap();

    let learned = h
        .pipeline
        .record_external_resolution(
            &ExternalRef::new("jira-acme", "OPS-3"),
            Some("echo of our own transition".into()),
            None,
        )
        .await
        .unwrap();

    assert!(learned.is_none());
    assert_eq!(h.pipeline.knowledge_size().await.unwrap(), before);
}

#[tokio::test]
async fn worker_pool_processes_queued_tickets_and_shuts_down() {
    let h = harness().await;
    let (queue, workers) = WorkerPool::spawn(h.pipeline.clone(), 2, 16);
    let mut ids = Vec::new();
    for n in 0..5 {
        let id = h
            .pipeline
            .ingest(new_ticket(&format!("Printer jam {n}"), "Floor 3", None))
            .await
            .unwrap()
            .ticket
            .id;
        queue.try_enqueue(id).unwrap();
        ids.push(id);
    }
    drop(queue);
    workers.join().await;

    for id in ids {
        let status = h.store.get(id).await.unwrap().unwrap().status;
        assert_eq!(status, TicketStatus::Escalated);
    }
}

#[tokio::test]
async fn recover_requeues_tickets_interrupted_mid_triage() {
    let h = harness().await;
    let id = h
        .pipeline
        .ingest(new_ticket("Printer jam", "Floor 3", None))
        .await
        .unwrap()
        .ticket
        .id;
    h.store
        .set_status(id, TicketStatus::Triaging)
        .await
        .unwrap();

    let pending = h.pipeline.recover(100).await.unwrap();

    assert_eq!(pending, vec![id]);
    assert_eq!(
        h.store.get(id).await.unwrap().unwrap().status,
        TicketStatus::Received
    );
}
