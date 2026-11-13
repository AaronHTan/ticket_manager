//! Seeds a file-backed SQLite database from the checked-in fixtures, the same
//! way `ticket-seed` does, and checks the result survives a reopen.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use ticket_core::TicketStatus;
use ticket_core::ports::TicketRepository;
use ticket_server::AppConfig;
use ticket_server::bootstrap::build_pipeline;
use ticket_server::seed::{FixtureTicket, load_history, load_tickets, seed};
use ticket_storage::SqliteStore;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `config/test.toml`, pointed at a database inside `dir`.
fn test_config(dir: &Path) -> (AppConfig, String) {
    let raw = std::fs::read_to_string(repo_root().join("config/test.toml")).unwrap();
    let mut config = AppConfig::from_toml(&raw).unwrap();
    let url = format!("sqlite://{}", dir.join("test.db").display());
    config.database.url.clone_from(&url);
    (config, url)
}

fn fixtures() -> (Vec<ticket_core::HistoricalTicket>, Vec<FixtureTicket>) {
    let root = repo_root();
    (
        load_history(&root.join("fixtures/history.json")).unwrap(),
        load_tickets(&root.join("fixtures/tickets.json")).unwrap(),
    )
}

#[tokio::test]
async fn fixtures_seed_a_file_database_with_expected_routes() {
    let dir = TempDir::new().unwrap();
    let (config, url) = test_config(dir.path());
    let (history, tickets) = fixtures();
    let history_len = history.len();
    let expected: Vec<TicketStatus> = tickets.iter().map(|t| t.expect).collect();

    // Every outcome is represented, so the fixtures exercise each route.
    for status in [
        TicketStatus::AutoResolved,
        TicketStatus::Forwarded,
        TicketStatus::Escalated,
        TicketStatus::Received,
    ] {
        assert!(expected.contains(&status), "no fixture expects {status}");
    }

    let pipeline = build_pipeline(&config).await.unwrap();
    let report = seed(&pipeline, history.clone(), tickets.clone())
        .await
        .unwrap();
    let failures: Vec<_> = report
        .mismatches()
        .map(|t| {
            format!(
                "{} `{}`: expected {}, got {}",
                t.key, t.title, t.expected, t.actual
            )
        })
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
    assert_eq!(report.imported, Some(history_len));
    assert!(report.tickets.iter().all(|t| t.created));

    // Seeding again changes nothing.
    let again = seed(&pipeline, history, tickets).await.unwrap();
    assert_eq!(again.imported, None);
    assert_eq!(again.knowledge_entries, report.knowledge_entries);
    assert!(again.tickets.iter().all(|t| !t.created && t.matches()));
    let ids =
        |r: &ticket_server::seed::SeedReport| r.tickets.iter().map(|t| t.id).collect::<Vec<_>>();
    assert_eq!(ids(&again), ids(&report));
    drop(pipeline);

    // A fresh connection to the same file sees everything.
    let store = SqliteStore::connect(&url).await.unwrap();
    for t in &report.tickets {
        let stored = store.get(t.id).await.unwrap().expect("ticket persisted");
        assert_eq!(stored.status, t.expected, "{}", t.key);
        let triage = store.latest_triage(t.id).await.unwrap();
        assert_eq!(
            triage.is_some(),
            t.expected != TicketStatus::Received,
            "{}: triage report presence",
            t.key
        );
    }
    let fx9 = report.tickets.iter().find(|t| t.key == "FX-9").unwrap();
    let fx9 = store.get(fx9.id).await.unwrap().unwrap();
    assert_eq!(
        fx9.code_context.unwrap().repository.unwrap().as_str(),
        "web-app"
    );

    // Server start-up recovery picks up the ticket left `received`.
    let pipeline = build_pipeline(&config).await.unwrap();
    let pending = pipeline.recover(100).await.unwrap();
    let received: Vec<_> = report
        .tickets
        .iter()
        .filter(|t| t.expected == TicketStatus::Received)
        .map(|t| t.id)
        .collect();
    assert_eq!(pending, received);
}

#[test]
fn fixture_tickets_must_be_idempotent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("tickets.json");
    std::fs::write(
        &path,
        r#"[{"ticket": {"client": "acme", "title": "no ref"}, "expect": "escalated"}]"#,
    )
    .unwrap();
    let err = load_tickets(&path).unwrap_err();
    assert!(err.to_string().contains("external_ref"), "{err}");

    std::fs::write(
        &path,
        r#"[{"ticket": {"client": "acme", "title": "x",
             "external_ref": {"system": "fixtures", "key": "X"}}, "expect": "failed"}]"#,
    )
    .unwrap();
    assert!(load_tickets(&path).is_err());
}

/// Runs `git` for fixture setup, with a fixed identity and no user config.
fn git(cwd: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "demo")
        .env("GIT_AUTHOR_EMAIL", "demo@example.test")
        .env("GIT_COMMITTER_NAME", "demo")
        .env("GIT_COMMITTER_EMAIL", "demo@example.test")
        .status()
        .expect("git is installed");
    assert!(status.success(), "git {args:?} failed");
}

/// The demo (`scripts/demo.sh`) stays correct: with `config/demo.toml` and a
/// repository made from `demo/repo`, FX-9 gets a proposed fix whose branch
/// contains the patched `cart.py`.
#[tokio::test]
async fn demo_fixtures_propose_a_fix_for_the_checkout_crash() {
    let dir = TempDir::new().unwrap();
    let root = repo_root();

    // The same bare remote the demo script builds.
    let remote = dir.path().join("web-app.git");
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    for file in std::fs::read_dir(root.join("demo/repo")).unwrap() {
        let file = file.unwrap();
        std::fs::copy(file.path(), work.join(file.file_name())).unwrap();
    }
    git(&work, &["init", "--quiet", "--initial-branch=main"]);
    git(&work, &["add", "--all"]);
    git(&work, &["commit", "--quiet", "-m", "Initial shop"]);
    git(
        dir.path(),
        &[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=main",
            remote.to_str().unwrap(),
        ],
    );
    git(
        &work,
        &["push", "--quiet", remote.to_str().unwrap(), "main"],
    );

    let raw = std::fs::read_to_string(root.join("config/demo.toml")).unwrap();
    let mut config = AppConfig::from_toml(&raw).unwrap();
    config.database.url = format!("sqlite://{}", dir.path().join("demo.db").display());
    config.git.cache_dir = dir.path().join("repos");
    config.git.workspace_dir = dir.path().join("workspaces");
    let web_app = ticket_core::RepoId::new("web-app").unwrap();
    config.repositories.get_mut(&web_app).unwrap().remote = remote.to_str().unwrap().to_owned();

    let pipeline = build_pipeline(&config).await.unwrap();
    let report = seed(
        &pipeline,
        load_history(&root.join("fixtures/history.json")).unwrap(),
        load_tickets(&root.join("demo/tickets.json")).unwrap(),
    )
    .await
    .unwrap();
    let failures: Vec<_> = report
        .mismatches()
        .map(|t| {
            format!(
                "{}: expected {} / {:?}, got {} / {}",
                t.key,
                t.expected,
                t.expected_fix,
                t.actual,
                t.fix_label()
            )
        })
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");

    let fx9 = report.tickets.iter().find(|t| t.key == "FX-9").unwrap();
    let Some(ticket_core::FixResult::Proposed { branch, .. }) = &fx9.fix else {
        panic!("FX-9 should have a proposed fix");
    };
    let shown = std::process::Command::new("git")
        .args(["--git-dir", remote.to_str().unwrap(), "show"])
        .arg(format!("refs/heads/{branch}:cart.py"))
        .output()
        .unwrap();
    let cart = String::from_utf8(shown.stdout).unwrap();
    assert!(cart.contains("for item in items or []"), "{cart}");
    // No workspace is left behind.
    assert_eq!(
        std::fs::read_dir(dir.path().join("workspaces"))
            .unwrap()
            .count(),
        0
    );
}
