//! Contract tests: every storage adapter must pass the same suite.

use chrono::{Duration, Utc};
use ticket_core::ports::{KnowledgeBase, TicketRepository};
use ticket_core::{
    Assignee, Category, Classification, ClientId, Embedding, Error, ExternalRef, KnowledgeEntry,
    NewTicket, Resolution, RoutingDecision, Severity, SeverityAssessment, Ticket, TicketId,
    TicketStatus, TriageReport,
};
use ticket_storage::{InMemoryStore, SqliteStore};

fn ticket(title: &str, external_key: Option<&str>, age_minutes: i64) -> Ticket {
    let new = NewTicket {
        client: ClientId::new("acme").unwrap(),
        external_ref: external_key.map(|k| ExternalRef::new("jira-acme", k)),
        title: title.into(),
        description: format!("{title} - full description"),
        reporter: Some("jane@acme.test".into()),
        labels: vec!["prod".into(), "eu-west".into()],
        reported_severity: Some(Severity::High),
    };
    Ticket::receive(new, Utc::now() - Duration::minutes(age_minutes)).unwrap()
}

fn report(ticket_id: TicketId, reason: &str) -> TriageReport {
    TriageReport {
        ticket_id,
        classification: Classification {
            severity: SeverityAssessment {
                severity: Severity::High,
                confidence: 0.7,
                rationale: vec!["keyword `crash`".into()],
            },
            category: Some(Category::new("database").unwrap()),
        },
        matches: vec![],
        decision: RoutingDecision::Escalate {
            assignee: Assignee {
                name: "DBA".into(),
                contact: Some("dba@acme.test".into()),
                external_id: None,
            },
            reason: reason.into(),
        },
        decided_at: Utc::now(),
    }
}

fn entry(title: &str, automatable: bool) -> KnowledgeEntry {
    KnowledgeEntry {
        ticket_id: TicketId::new(),
        title: title.into(),
        description: String::new(),
        category: Some(Category::new("authentication").unwrap()),
        severity: Severity::Low,
        resolution: Resolution {
            summary: format!("fix for {title}"),
            automatable,
            resolved_by: Some("l2".into()),
        },
        updated_at: Utc::now(),
    }
}

async fn ticket_repository_contract(store: &dyn TicketRepository) {
    let older = ticket("Login page crashes", Some("OPS-1"), 10);
    let newer = ticket("Database timeout", None, 1);
    store.insert(&newer).await.unwrap();
    store.insert(&older).await.unwrap();

    // Round trip preserves every field.
    assert_eq!(store.get(older.id).await.unwrap().as_ref(), Some(&older));
    assert_eq!(store.get(TicketId::new()).await.unwrap(), None);

    // Duplicate ids and duplicate external references conflict.
    assert!(matches!(
        store.insert(&older).await,
        Err(Error::Conflict(_))
    ));
    let duplicate_external = ticket("Same Jira issue", Some("OPS-1"), 0);
    assert!(matches!(
        store.insert(&duplicate_external).await,
        Err(Error::Conflict(_))
    ));

    let found = store
        .find_by_external_ref(&ExternalRef::new("jira-acme", "OPS-1"))
        .await
        .unwrap();
    assert_eq!(found.map(|t| t.id), Some(older.id));

    // Received tickets list oldest first.
    let received = store
        .list_by_status(TicketStatus::Received, 10)
        .await
        .unwrap();
    assert_eq!(
        received.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![older.id, newer.id]
    );
    assert_eq!(
        store
            .list_by_status(TicketStatus::Received, 1)
            .await
            .unwrap()
            .len(),
        1
    );

    // A claim succeeds once and only from an allowed state.
    let claim = [TicketStatus::Received, TicketStatus::Failed];
    assert!(
        store
            .transition(older.id, &claim, TicketStatus::Triaging)
            .await
            .unwrap()
    );
    assert!(
        !store
            .transition(older.id, &claim, TicketStatus::Triaging)
            .await
            .unwrap()
    );
    assert!(
        !store
            .transition(TicketId::new(), &claim, TicketStatus::Triaging)
            .await
            .unwrap()
    );
    assert!(
        !store
            .transition(newer.id, &[], TicketStatus::Triaging)
            .await
            .unwrap()
    );
    assert_eq!(
        store.get(older.id).await.unwrap().unwrap().status,
        TicketStatus::Triaging
    );

    store
        .set_status(newer.id, TicketStatus::Escalated)
        .await
        .unwrap();
    assert!(matches!(
        store
            .set_status(TicketId::new(), TicketStatus::Failed)
            .await,
        Err(Error::NotFound(_))
    ));

    // Triage reports are append-only and the latest wins.
    assert_eq!(store.latest_triage(older.id).await.unwrap(), None);
    store.save_triage(&report(older.id, "first")).await.unwrap();
    store
        .save_triage(&report(older.id, "second"))
        .await
        .unwrap();
    let latest = store.latest_triage(older.id).await.unwrap().unwrap();
    assert!(
        matches!(latest.decision, RoutingDecision::Escalate { ref reason, .. } if reason == "second")
    );
    assert!(matches!(
        store.save_triage(&report(TicketId::new(), "orphan")).await,
        Err(Error::NotFound(_))
    ));
}

async fn knowledge_base_contract(kb: &dyn KnowledgeBase) {
    assert_eq!(kb.count().await.unwrap(), 0);
    let close = entry("Password reset", true);
    let far = entry("Billing question", false);
    kb.upsert(&close, &Embedding::new(vec![1.0, 0.1, 0.0]))
        .await
        .unwrap();
    kb.upsert(&far, &Embedding::new(vec![0.0, 0.0, 1.0]))
        .await
        .unwrap();
    assert_eq!(kb.count().await.unwrap(), 2);

    let hits = kb
        .search(&Embedding::new(vec![1.0, 0.0, 0.0]), 5)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].entry, close);
    assert!(hits[0].score > 0.99 && hits[1].score.abs() < 1e-6);
    assert_eq!(
        kb.search(&Embedding::new(vec![1.0, 0.0, 0.0]), 1)
            .await
            .unwrap()
            .len(),
        1
    );

    // Upsert replaces in place.
    let mut updated = close.clone();
    updated.resolution.summary = "better fix".into();
    kb.upsert(&updated, &Embedding::new(vec![1.0, 0.1, 0.0]))
        .await
        .unwrap();
    assert_eq!(kb.count().await.unwrap(), 2);
    let hits = kb
        .search(&Embedding::new(vec![1.0, 0.0, 0.0]), 1)
        .await
        .unwrap();
    assert_eq!(hits[0].entry.resolution.summary, "better fix");

    // Vectors of a different dimension never match.
    let hits = kb.search(&Embedding::new(vec![1.0, 0.0]), 5).await.unwrap();
    assert!(hits.iter().all(|h| h.score == 0.0));
}

#[tokio::test]
async fn in_memory_store_satisfies_contract() {
    let store = InMemoryStore::new();
    ticket_repository_contract(&store).await;
    knowledge_base_contract(&store).await;
}

#[tokio::test]
async fn sqlite_store_satisfies_contract() {
    let store = SqliteStore::in_memory().await.unwrap();
    ticket_repository_contract(&store).await;
    knowledge_base_contract(&store).await;
}
