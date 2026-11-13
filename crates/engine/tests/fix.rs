//! `FixCoordinator` end to end, with the real git adapter and a local
//! bare repository standing in for the remote.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use tempfile::TempDir;
use ticket_core::ports::{CodeHost, Fixer};
use ticket_core::{
    ChangeRequest, ChangeRequestRef, ClientId, Error, ExternalRef, FixOutcome, FixResult,
    NewTicket, RepoId, Result, Signature, Ticket, Workspace,
};
use ticket_engine::{
    FixCoordinator, LogCodeHost, NoopFixer, RepositoryConfig, RepositoryDirectory,
};
use ticket_git::{GitCli, GitConfig};

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

/// A bare remote with one commit on `main`.
fn remote(dir: &Path) -> String {
    let seed = dir.join("seed");
    let remote = dir.join("remote.git");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["-c", "init.defaultBranch=main", "init", "--quiet"]);
    std::fs::write(
        seed.join("cart.py"),
        "def total(items):\n    return sum(items)\n",
    )
    .unwrap();
    git(&seed, &["add", "--all"]);
    git(&seed, &["commit", "--quiet", "-m", "initial"]);
    let remote = remote.to_str().unwrap().to_owned();
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

/// Writes a guard into `cart.py`, the way a real fixer would edit code.
struct PatchFixer;

#[async_trait]
impl Fixer for PatchFixer {
    async fn attempt(&self, ticket: &Ticket, workspace: &Workspace) -> Result<FixOutcome> {
        assert_eq!(workspace.ticket_id, ticket.id);
        std::fs::write(
            workspace.path.join("cart.py"),
            "def total(items):\n    return sum(items or [])\n",
        )
        .map_err(|e| Error::Validation(e.to_string()))?;
        Ok(FixOutcome {
            summary: "Treat a missing cart as empty.".into(),
        })
    }
}

/// Fails after touching the tree, to check the workspace is still released.
struct FailingFixer;

#[async_trait]
impl Fixer for FailingFixer {
    async fn attempt(&self, _ticket: &Ticket, workspace: &Workspace) -> Result<FixOutcome> {
        std::fs::write(workspace.path.join("half-done.txt"), "x").unwrap();
        Err(Error::Unavailable("model timed out".into()))
    }
}

/// Records change requests so the test can inspect them.
#[derive(Default)]
struct RecordingHost(Mutex<Vec<ChangeRequest>>);

#[async_trait]
impl CodeHost for RecordingHost {
    async fn open_change_request(&self, request: &ChangeRequest) -> Result<ChangeRequestRef> {
        self.0.lock().unwrap().push(request.clone());
        Ok(ChangeRequestRef {
            id: "42".into(),
            url: Some("https://git.test/acme/shop/pull/42".into()),
        })
    }
}

struct Setup {
    dir: TempDir,
    remote: String,
    git: Arc<GitCli>,
    directory: RepositoryDirectory,
}

fn setup() -> Setup {
    let dir = TempDir::new().unwrap();
    let remote = remote(dir.path());
    let git = Arc::new(
        GitCli::new(&GitConfig {
            cache_dir: dir.path().join("cache"),
            workspace_dir: dir.path().join("workspaces"),
            ..GitConfig::default()
        })
        .unwrap(),
    );
    let directory = RepositoryDirectory::new(BTreeMap::from([(
        RepoId::new("shop").unwrap(),
        RepositoryConfig {
            remote: remote.clone(),
            default_branch: "main".parse().unwrap(),
            clients: vec![ClientId::new("acme").unwrap()],
            components: vec!["checkout".into()],
        },
    )]))
    .unwrap();
    Setup {
        dir,
        remote,
        git,
        directory,
    }
}

fn author() -> Signature {
    Signature {
        name: "ticket-handler".into(),
        email: "bot@test".into(),
    }
}

fn ticket(client: &str) -> Ticket {
    let new = NewTicket::new(
        ClientId::new(client).unwrap(),
        "Checkout crashes on empty cart",
        "TypeError: 'NoneType' object is not iterable",
    )
    .with_external_ref(ExternalRef::new("jira-acme", "SHOP-7"))
    .with_components(["checkout"]);
    Ticket::receive(new, Utc::now()).unwrap()
}

fn workspaces_left(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("workspaces")).unwrap().count()
}

#[tokio::test]
async fn fix_is_committed_pushed_and_proposed() {
    let s = setup();
    let host = Arc::new(RecordingHost::default());
    let coordinator = FixCoordinator::new(
        s.directory.clone(),
        s.git.clone(),
        host.clone(),
        Arc::new(PatchFixer),
        author(),
    );
    let t = ticket("acme");

    let FixResult::Proposed {
        repo,
        branch,
        commit,
        change_request,
        summary,
        diff,
    } = coordinator.run(&t).await.unwrap()
    else {
        panic!("expected a proposed fix");
    };
    assert_eq!(repo.as_str(), "shop");
    assert_eq!(branch.as_str(), format!("ticket/{}", t.id));
    assert_eq!(summary, "Treat a missing cart as empty.");
    assert!(diff.contains("+    return sum(items or [])"), "{diff}");
    assert_eq!(
        change_request.url.as_deref(),
        Some("https://git.test/acme/shop/pull/42")
    );

    // The remote has the branch, with the fix and a descriptive commit.
    let remote = Path::new(&s.remote);
    let branch_ref = format!("refs/heads/{branch}");
    assert_eq!(git(remote, &["rev-parse", &branch_ref]), commit.as_str());
    assert_eq!(
        git(remote, &["show", &format!("{branch_ref}:cart.py")]),
        "def total(items):\n    return sum(items or [])"
    );
    let message = git(remote, &["log", "-1", "--format=%B", &branch_ref]);
    assert!(message.starts_with("Fix SHOP-7: Checkout crashes on empty cart"));
    assert!(message.ends_with("Ticket: SHOP-7"));
    // main is untouched.
    assert_eq!(
        git(remote, &["rev-list", "--count", "refs/heads/main"]),
        "1"
    );

    let requests = host.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].base.as_str(), "main");
    assert_eq!(requests[0].branch, branch);
    assert!(requests[0].body.contains("Treat a missing cart as empty."));

    assert_eq!(workspaces_left(s.dir.path()), 0);
}

#[tokio::test]
async fn noop_fixer_changes_nothing_and_cleans_up() {
    let s = setup();
    let coordinator = FixCoordinator::new(
        s.directory.clone(),
        s.git.clone(),
        Arc::new(LogCodeHost),
        Arc::new(NoopFixer),
        author(),
    );
    let result = coordinator.run(&ticket("acme")).await.unwrap();
    assert!(matches!(result, FixResult::NoChanges { ref repo, .. } if repo.as_str() == "shop"));
    assert_eq!(workspaces_left(s.dir.path()), 0);
    // Nothing was pushed.
    assert_eq!(
        git(
            Path::new(&s.remote),
            &["for-each-ref", "--format=%(refname)"]
        ),
        "refs/heads/main"
    );
}

#[tokio::test]
async fn failed_attempt_still_releases_the_workspace() {
    let s = setup();
    let coordinator = FixCoordinator::new(
        s.directory.clone(),
        s.git.clone(),
        Arc::new(LogCodeHost),
        Arc::new(FailingFixer),
        author(),
    );
    let t = ticket("acme");
    assert!(matches!(
        coordinator.run(&t).await,
        Err(Error::Unavailable(_))
    ));
    assert_eq!(workspaces_left(s.dir.path()), 0);

    // The same ticket can be retried afterwards.
    let retry = FixCoordinator::new(
        s.directory.clone(),
        s.git.clone(),
        Arc::new(LogCodeHost),
        Arc::new(PatchFixer),
        author(),
    );
    assert!(matches!(
        retry.run(&t).await.unwrap(),
        FixResult::Proposed { .. }
    ));
}

#[tokio::test]
async fn tickets_without_a_permitted_repository_are_skipped() {
    let s = setup();
    let coordinator = FixCoordinator::new(
        s.directory.clone(),
        s.git.clone(),
        Arc::new(LogCodeHost),
        Arc::new(PatchFixer),
        author(),
    );
    // `shop` is restricted to acme.
    assert_eq!(
        coordinator.run(&ticket("globex")).await.unwrap(),
        FixResult::NoRepository
    );
    // Nothing was even cloned.
    assert!(!s.git.mirror_path(&RepoId::new("shop").unwrap()).exists());
}
