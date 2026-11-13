//! Contract tests: every storage adapter must pass the same suite.

use chrono::{Duration, TimeZone, Utc};
use ticket_core::ports::{KnowledgeBase, TicketRepository};
use ticket_core::{
    Assignee, BranchName, Category, ChangeRequestRef, Classification, ClientId, CodeContext,
    CommitId, Embedding, Error, ExternalRef, FixAttempt, FixResult, KnowledgeEntry, Link,
    NewTicket, RepoId, Reporter, Resolution, RoutingDecision, Severity, SeverityAssessment, Ticket,
    TicketFilter, TicketId, TicketKind, TicketStatus, TriageReport,
};
use ticket_storage::{InMemoryStore, SqliteStore};

fn ticket(title: &str, external_key: Option<&str>, age_minutes: i64) -> Ticket {
    let mut new = NewTicket::new(
        ClientId::new("acme").unwrap(),
        title,
        format!("{title} - full description"),
    )
    .with_reporter(Reporter {
        name: Some("Jane Doe".into()),
        email: Some("jane@acme.test".into()),
        external_id: Some("5b10ac8d82e05b22cc7d4ef5".into()),
    })
    .with_kind(TicketKind::Bug)
    .with_labels(["prod", "eu-west"])
    .with_components(["auth", "frontend"])
    .with_reported_severity(Severity::High)
    .with_code_context(CodeContext {
        repository: Some(RepoId::new("web-app").unwrap()),
        git_ref: Some("release/2.4".into()),
        paths: vec!["src/login.rs".into()],
        version: Some("2.4.1".into()),
    })
    .with_link(Link {
        url: "https://logs.acme.test/trace/91".into(),
        title: Some("stack trace".into()),
    });
    if let Some(key) = external_key {
        new = new.with_external_ref(ExternalRef::new("jira-acme", key));
    }
    new.reported_at = Some(Utc.with_ymd_and_hms(2026, 11, 1, 9, 30, 0).unwrap());
    new.metadata.insert("jira.issuetype".into(), "Bug".into());
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
        draft: Some(ticket_core::DraftResolution {
            text: "Preliminary assessment".into(),
            first_steps: vec!["Check replication lag.".into()],
            precedents: vec![ticket_core::Precedent {
                ticket_id: TicketId::new(),
                title: "Replication lag".into(),
                score: 0.4,
                resolution: "Moved the analytics query.".into(),
            }],
        }),
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
            patch: automatable
                .then(|| "--- a/login.rs\n+++ b/login.rs\n@@ -1 +1 @@\n-old\n+new\n".to_owned()),
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

    // Listing is newest first and filters by status and client.
    let all = store.list(&TicketFilter::default()).await.unwrap();
    assert_eq!(
        all.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![newer.id, older.id]
    );
    let limited = TicketFilter {
        limit: 1,
        ..TicketFilter::default()
    };
    assert_eq!(store.list(&limited).await.unwrap().len(), 1);
    let other_client = TicketFilter {
        client: Some(ClientId::new("globex").unwrap()),
        ..TicketFilter::default()
    };
    assert!(store.list(&other_client).await.unwrap().is_empty());
    let acme = TicketFilter {
        client: Some(ClientId::new("acme").unwrap()),
        ..TicketFilter::default()
    };
    assert_eq!(store.list(&acme).await.unwrap().len(), 2);

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
    let escalated = TicketFilter {
        status: Some(TicketStatus::Escalated),
        ..TicketFilter::default()
    };
    assert_eq!(
        store
            .list(&escalated)
            .await
            .unwrap()
            .iter()
            .map(|t| t.id)
            .collect::<Vec<_>>(),
        vec![newer.id]
    );
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

    // Fix attempts are append-only and the latest wins.
    assert_eq!(store.latest_fix_attempt(older.id).await.unwrap(), None);
    let failed = FixAttempt {
        ticket_id: older.id,
        result: FixResult::Failed {
            error: "push rejected".into(),
        },
        attempted_at: Utc::now(),
    };
    let proposed = FixAttempt {
        ticket_id: older.id,
        result: FixResult::Proposed {
            repo: RepoId::new("web-app").unwrap(),
            branch: BranchName::for_ticket(older.id),
            commit: CommitId::new("0123456789abcdef").unwrap(),
            change_request: ChangeRequestRef {
                id: "42".into(),
                url: Some("https://git.test/pr/42".into()),
            },
            summary: "guard empty cart".into(),
            diff: String::new(),
        },
        attempted_at: Utc::now(),
    };
    store.save_fix_attempt(&failed).await.unwrap();
    store.save_fix_attempt(&proposed).await.unwrap();
    assert_eq!(
        store.latest_fix_attempt(older.id).await.unwrap(),
        Some(proposed)
    );
    assert!(matches!(
        store
            .save_fix_attempt(&FixAttempt {
                ticket_id: TicketId::new(),
                ..failed
            })
            .await,
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

#[tokio::test]
async fn sqlite_reads_rows_written_before_metadata_migration() {
    let store = SqliteStore::in_memory().await.unwrap();
    let id = TicketId::new();
    // The column set of migration 0001: no kind, reported_at or details, and a
    // plain-text reporter.
    sqlx::query(
        "INSERT INTO tickets (id, client_id, title, description, reporter, labels, status, \
         created_at, updated_at) VALUES (?, 'acme', 'Old ticket', '', 'Jane Doe', '[]', \
         'received', ?, ?)",
    )
    .bind(id.to_string())
    .bind(Utc::now())
    .bind(Utc::now())
    .execute(store.pool())
    .await
    .unwrap();

    let ticket = store.get(id).await.unwrap().unwrap();
    assert_eq!(ticket.reporter.unwrap().name.as_deref(), Some("Jane Doe"));
    assert_eq!(ticket.kind, TicketKind::Other);
    assert!(ticket.components.is_empty() && ticket.links.is_empty());
    assert_eq!(ticket.code_context, None);
    assert_eq!(ticket.reported_at, None);
}
