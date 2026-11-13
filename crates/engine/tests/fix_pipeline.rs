//! Triage followed by an automatic fix: the real pipeline, the real git
//! adapter, and a `PatchFixer` re-applying a patch learned from history.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tempfile::TempDir;
use ticket_core::ports::Tracker;
use ticket_core::{
    Assignee, Category, ClientId, ExternalRef, FixResult, HistoricalTicket, NewTicket, RepoId,
    Resolution, Result, Severity, Signature, TicketKind, TicketStatus,
};
use ticket_engine::{
    Dispatcher, EscalationDirectory, FixCoordinator, FixPolicy, LogCodeHost, LogNotifier,
    PatchFixer, PipelineDeps, RepositoryConfig, RepositoryDirectory, RoutingPolicy,
    RuleBasedClassifier, TicketRouter, TriagePipeline,
};
use ticket_git::{GitCli, GitConfig};
use ticket_retrieval::HashingEmbedder;
use ticket_storage::InMemoryStore;

const BUGGY: &str = "def total(items):\n    return sum(items)\n";
const FIXED: &str = "def total(items):\n    return sum(items or [])\n";
const PATCH: &str = "--- a/cart.py\n+++ b/cart.py\n@@ -1,2 +1,2 @@\n def total(items):\n-    return sum(items)\n+    return sum(items or [])\n";

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@test")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@test")
        .output()
        .expect("git is installed");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn remote_with(dir: &Path, cart: &str) -> String {
    let seed = dir.join("seed");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["-c", "init.defaultBranch=main", "init", "--quiet"]);
    std::fs::write(seed.join("cart.py"), cart).unwrap();
    git(&seed, &["add", "--all"]);
    git(&seed, &["commit", "--quiet", "-m", "initial"]);
    let remote = dir.join("remote.git").to_str().unwrap().to_owned();
    git(
        dir,
        &[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=main",
            &remote,
        ],
    );
    git(&seed, &["push", "--quiet", &remote, "main"]);
    remote
}

#[derive(Default)]
struct RecordingTracker(Mutex<Vec<String>>);

#[async_trait]
impl Tracker for RecordingTracker {
    fn system(&self) -> &str {
        "jira-acme"
    }
    async fn comment(&self, external: &ExternalRef, body: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .push(format!("{}: {body}", external.key));
        Ok(())
    }
    async fn assign(&self, _: &ExternalRef, _: &Assignee) -> Result<()> {
        Ok(())
    }
    async fn set_severity(&self, _: &ExternalRef, _: Severity) -> Result<()> {
        Ok(())
    }
    async fn mark_resolved(&self, _: &ExternalRef, _: &str) -> Result<()> {
        Ok(())
    }
}

struct Harness {
    _dir: TempDir,
    remote: String,
    pipeline: TriagePipeline,
    tracker: Arc<RecordingTracker>,
}

async fn harness(cart: &str) -> Harness {
    let dir = TempDir::new().unwrap();
    let remote = remote_with(dir.path(), cart);
    let store = Arc::new(InMemoryStore::new());
    let embedder = Arc::new(HashingEmbedder::default());
    let git = Arc::new(
        GitCli::new(&GitConfig {
            cache_dir: dir.path().join("cache"),
            workspace_dir: dir.path().join("workspaces"),
            ..GitConfig::default()
        })
        .unwrap(),
    );
    let policy = FixPolicy {
        enabled: true,
        ..FixPolicy::default()
    };
    let coordinator = FixCoordinator::new(
        RepositoryDirectory::new(BTreeMap::from([(
            RepoId::new("shop").unwrap(),
            RepositoryConfig {
                remote: remote.clone(),
                default_branch: "main".parse().unwrap(),
                clients: vec![],
                components: vec!["checkout".into()],
            },
        )]))
        .unwrap(),
        git.clone(),
        Arc::new(LogCodeHost),
        Arc::new(PatchFixer::new(
            store.clone(),
            embedder.clone(),
            git,
            policy.min_similarity,
        )),
        Signature {
            name: "ticket-handler".into(),
            email: "bot@test".into(),
        },
    );
    let assignee = |name: &str| Assignee {
        name: name.into(),
        contact: None,
        external_id: None,
    };
    let router = TicketRouter::new(
        RoutingPolicy::default(),
        EscalationDirectory {
            intermediate: assignee("L2"),
            default_expert: assignee("Platform"),
            experts: HashMap::new(),
        },
    )
    .unwrap();
    let tracker = Arc::new(RecordingTracker::default());
    let pipeline = TriagePipeline::new(
        PipelineDeps {
            tickets: store.clone(),
            knowledge: store,
            embedder,
            classifier: Arc::new(RuleBasedClassifier::default()),
        },
        router,
        Dispatcher::new(Arc::new(LogNotifier)).with_tracker(tracker.clone()),
    )
    .with_fixes(Arc::new(coordinator), policy);

    pipeline
        .import_history(vec![HistoricalTicket {
            title: "Checkout returns 500 on empty cart".into(),
            description: "Submitting checkout with an empty cart crashes with a 500 error.".into(),
            category: Some(Category::new("billing").unwrap()),
            severity: Severity::High,
            resolution: Resolution {
                summary: "Guard against an empty cart in cart.total.".into(),
                automatable: false,
                resolved_by: Some("web-team".into()),
                patch: Some(PATCH.into()),
            },
        }])
        .await
        .unwrap();

    Harness {
        _dir: dir,
        remote,
        pipeline,
        tracker,
    }
}

fn checkout_crash(kind: TicketKind, key: &str) -> NewTicket {
    NewTicket::new(
        ClientId::new("acme").unwrap(),
        "Checkout returns 500 on empty cart again",
        "Submitting checkout with an empty cart crashes with a 500 error.",
    )
    .with_kind(kind)
    .with_components(["checkout"])
    .with_external_ref(ExternalRef::new("jira-acme", key))
}

#[tokio::test]
async fn similar_bug_gets_the_learned_patch_pushed_for_review() {
    let h = harness(BUGGY).await;
    let ticket = h
        .pipeline
        .ingest(checkout_crash(TicketKind::Bug, "SHOP-12"))
        .await
        .unwrap()
        .ticket;
    let report = h.pipeline.process(ticket.id).await.unwrap();

    // Triage is unchanged by the fix: a known issue goes to L2.
    assert_eq!(report.decision.resulting_status(), TicketStatus::Forwarded);

    let attempt = h.pipeline.latest_fix(ticket.id).await.unwrap().unwrap();
    let FixResult::Proposed {
        repo,
        branch,
        commit,
        summary,
        diff,
        ..
    } = attempt.result
    else {
        panic!("expected a proposed fix, got {:?}", attempt.result);
    };
    assert_eq!(repo.as_str(), "shop");
    assert!(diff.contains("+    return sum(items or [])"), "{diff}");
    assert!(diff.contains("-    return sum(items)"), "{diff}");
    assert!(
        summary.contains("Checkout returns 500 on empty cart"),
        "{summary}"
    );

    let remote = Path::new(&h.remote);
    let branch_ref = format!("refs/heads/{branch}");
    assert_eq!(git(remote, &["rev-parse", &branch_ref]), commit.as_str());
    assert_eq!(
        git(remote, &["show", &format!("{branch_ref}:cart.py")]),
        FIXED.trim_end()
    );

    let comments = h.tracker.0.lock().unwrap();
    assert!(
        comments
            .iter()
            .any(|c| c.starts_with("SHOP-12: [triage] Proposed a code fix")),
        "{comments:#?}"
    );
}

#[tokio::test]
async fn ineligible_kinds_get_no_fix_attempt() {
    let h = harness(BUGGY).await;
    let ticket = h
        .pipeline
        .ingest(checkout_crash(TicketKind::Question, "SHOP-13"))
        .await
        .unwrap()
        .ticket;
    h.pipeline.process(ticket.id).await.unwrap();
    assert_eq!(h.pipeline.latest_fix(ticket.id).await.unwrap(), None);
}

#[tokio::test]
async fn patch_that_no_longer_applies_is_recorded_as_no_changes() {
    // The code has moved on since the precedent was fixed.
    let h = harness("def total(items, tax=0):\n    return sum(items) * (1 + tax)\n").await;
    let ticket = h
        .pipeline
        .ingest(checkout_crash(TicketKind::Bug, "SHOP-14"))
        .await
        .unwrap()
        .ticket;
    h.pipeline.process(ticket.id).await.unwrap();

    let attempt = h.pipeline.latest_fix(ticket.id).await.unwrap().unwrap();
    let FixResult::NoChanges { summary, .. } = attempt.result else {
        panic!("expected no changes, got {:?}", attempt.result);
    };
    assert!(summary.contains("none applied cleanly"), "{summary}");
    // Nothing but main on the remote.
    assert_eq!(
        git(
            Path::new(&h.remote),
            &["for-each-ref", "--format=%(refname)"]
        ),
        "refs/heads/main"
    );
}

#[tokio::test]
async fn fix_step_can_be_run_on_demand_and_explains_refusals() {
    let h = harness(BUGGY).await;

    let question = h
        .pipeline
        .ingest(checkout_crash(TicketKind::Question, "SHOP-20"))
        .await
        .unwrap()
        .ticket;
    let early = h.pipeline.fix_ticket(question.id).await.unwrap_err();
    assert!(early.to_string().contains("triage the ticket"), "{early}");
    h.pipeline.triage_only(question.id).await.unwrap();
    let refused = h.pipeline.fix_ticket(question.id).await.unwrap_err();
    assert!(refused.to_string().contains("kind `question`"), "{refused}");

    // triage_only never runs the fix, even for an eligible ticket.
    let bug = h
        .pipeline
        .ingest(checkout_crash(TicketKind::Bug, "SHOP-21"))
        .await
        .unwrap()
        .ticket;
    h.pipeline.triage_only(bug.id).await.unwrap();
    assert_eq!(h.pipeline.latest_fix(bug.id).await.unwrap(), None);
    let attempt = h.pipeline.fix_ticket(bug.id).await.unwrap();
    assert_eq!(attempt.result.kind(), "proposed");
    assert_eq!(h.pipeline.latest_fix(bug.id).await.unwrap(), Some(attempt));
}
