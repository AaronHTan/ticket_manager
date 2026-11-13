//! Exercises `GitCli` against real repositories in a temporary directory.
//! The "remote" is a local bare repository, so no network is needed.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use ticket_core::ports::SourceControl;
use ticket_core::{BranchName, Error, RepoId, RepoSpec, Signature, TicketId};
use ticket_git::{GitCli, GitConfig};

struct Fixture {
    _dir: TempDir,
    remote: PathBuf,
    git: GitCli,
    spec: RepoSpec,
}

/// Runs git outside the adapter, with a fixed identity and no user config.
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

fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let seed = dir.path().join("seed");
    let remote = dir.path().join("remote.git");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["-c", "init.defaultBranch=main", "init", "--quiet"]);
    std::fs::write(seed.join("README.md"), "# web-app\n").unwrap();
    std::fs::write(seed.join("login.rs"), "fn login() {}\n").unwrap();
    git(&seed, &["add", "--all"]);
    git(&seed, &["commit", "--quiet", "-m", "initial"]);
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
        &seed,
        &["push", "--quiet", remote.to_str().unwrap(), "main"],
    );

    let config = GitConfig {
        cache_dir: dir.path().join("cache"),
        workspace_dir: dir.path().join("workspaces"),
        command_timeout_secs: 60,
        ..GitConfig::default()
    };
    let git = GitCli::new(&config).unwrap();
    let spec = RepoSpec {
        id: RepoId::new("web-app").unwrap(),
        remote: remote.to_str().unwrap().to_owned(),
        default_branch: BranchName::new("main").unwrap(),
    };
    Fixture {
        _dir: dir,
        remote,
        git,
        spec,
    }
}

fn author() -> Signature {
    Signature {
        name: "ticket-handler".into(),
        email: "bot@test".into(),
    }
}

#[tokio::test]
async fn full_workspace_lifecycle() {
    let f = fixture();
    assert!(f.git.version().await.unwrap().starts_with("git version"));

    let ticket = TicketId::new();
    let branch = BranchName::for_ticket(ticket);
    let ws = f.git.prepare(&f.spec, ticket, &branch).await.unwrap();
    assert!(ws.path.is_absolute());
    assert!(ws.path.join("login.rs").exists());
    assert_eq!(
        ws.base_commit.as_str(),
        git(&f.remote, &["rev-parse", "refs/heads/main"])
    );

    // A clean tree has nothing to diff or commit.
    assert_eq!(f.git.diff(&ws).await.unwrap(), "");
    assert_eq!(f.git.commit(&ws, "nothing", &author()).await.unwrap(), None);

    // Modified and new files both show up.
    std::fs::write(ws.path.join("login.rs"), "fn login() { check(); }\n").unwrap();
    std::fs::write(ws.path.join("check.rs"), "fn check() {}\n").unwrap();
    let diff = f.git.diff(&ws).await.unwrap();
    assert!(diff.contains("+fn login() { check(); }"), "{diff}");
    assert!(diff.contains("new file mode"), "{diff}");
    assert!(diff.contains("+fn check() {}"), "{diff}");

    let commit = f
        .git
        .commit(&ws, "Fix login check\n\nTicket: OPS-1", &author())
        .await
        .unwrap()
        .expect("changes were committed");
    assert_ne!(commit, ws.base_commit);
    assert_eq!(f.git.commit(&ws, "again", &author()).await.unwrap(), None);
    assert_eq!(
        git(&ws.path, &["log", "-1", "--format=%an <%ae>|%s"]),
        "ticket-handler <bot@test>|Fix login check"
    );
    // The diff is against the base, so it still shows the committed change.
    assert!(f.git.diff(&ws).await.unwrap().contains("check.rs"));

    f.git.push(&ws).await.unwrap();
    assert_eq!(
        git(
            &f.remote,
            &["rev-parse", &format!("refs/heads/{}", ws.branch)]
        ),
        commit.as_str()
    );

    f.git.release(&ws).await.unwrap();
    assert!(!ws.path.exists());
    let mirror = f.git.mirror_path(&f.spec.id);
    assert!(!git(&mirror, &["worktree", "list"]).contains(&ticket.to_string()));
    assert_eq!(git(&mirror, &["branch", "--list", branch.as_str()]), "");
    // The pushed branch survives on the remote.
    git(
        &f.remote,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    );
}

#[tokio::test]
async fn new_workspaces_start_from_the_latest_remote_commit() {
    let f = fixture();
    let first = TicketId::new();
    let ws = f
        .git
        .prepare(&f.spec, first, &BranchName::for_ticket(first))
        .await
        .unwrap();
    f.git.release(&ws).await.unwrap();

    // Someone else pushes to main.
    let other = f._dir.path().join("other");
    git(
        f._dir.path(),
        &[
            "clone",
            "--quiet",
            f.remote.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    std::fs::write(other.join("new.txt"), "new\n").unwrap();
    git(&other, &["add", "--all"]);
    git(&other, &["commit", "--quiet", "-m", "upstream change"]);
    git(&other, &["push", "--quiet", "origin", "main"]);

    let second = TicketId::new();
    let ws = f
        .git
        .prepare(&f.spec, second, &BranchName::for_ticket(second))
        .await
        .unwrap();
    assert!(ws.path.join("new.txt").exists());
    assert_eq!(
        ws.base_commit.as_str(),
        git(&f.remote, &["rev-parse", "refs/heads/main"])
    );
}

#[tokio::test]
async fn concurrent_workspaces_on_one_repository() {
    let f = fixture();
    let (a, b) = (TicketId::new(), TicketId::new());
    let (branch_a, branch_b) = (BranchName::for_ticket(a), BranchName::for_ticket(b));
    let (ws_a, ws_b) = tokio::join!(
        f.git.prepare(&f.spec, a, &branch_a),
        f.git.prepare(&f.spec, b, &branch_b),
    );
    let (ws_a, ws_b) = (ws_a.unwrap(), ws_b.unwrap());
    assert_ne!(ws_a.path, ws_b.path);

    std::fs::write(ws_a.path.join("a.txt"), "a\n").unwrap();
    std::fs::write(ws_b.path.join("b.txt"), "b\n").unwrap();
    let author = author();
    let (ca, cb) = tokio::join!(
        f.git.commit(&ws_a, "a", &author),
        f.git.commit(&ws_b, "b", &author),
    );
    assert!(ca.unwrap().is_some() && cb.unwrap().is_some());
    let (pa, pb) = tokio::join!(f.git.push(&ws_a), f.git.push(&ws_b));
    pa.unwrap();
    pb.unwrap();
    let (ra, rb) = tokio::join!(f.git.release(&ws_a), f.git.release(&ws_b));
    ra.unwrap();
    rb.unwrap();
}

#[tokio::test]
async fn conflicts_and_missing_branches_are_reported() {
    let f = fixture();
    let ticket = TicketId::new();
    let branch = BranchName::for_ticket(ticket);
    let ws = f.git.prepare(&f.spec, ticket, &branch).await.unwrap();

    // Same ticket again while its workspace is live.
    assert!(matches!(
        f.git.prepare(&f.spec, ticket, &branch).await,
        Err(Error::Conflict(_))
    ));
    // Another ticket asking for a branch that is checked out.
    assert!(matches!(
        f.git.prepare(&f.spec, TicketId::new(), &branch).await,
        Err(Error::Conflict(_))
    ));
    f.git.release(&ws).await.unwrap();

    // After release the ticket can start over.
    let ws = f.git.prepare(&f.spec, ticket, &branch).await.unwrap();
    f.git.release(&ws).await.unwrap();

    let missing = RepoSpec {
        default_branch: BranchName::new("does-not-exist").unwrap(),
        ..f.spec.clone()
    };
    let other = TicketId::new();
    assert!(matches!(
        f.git
            .prepare(&missing, other, &BranchName::for_ticket(other))
            .await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn repository_hooks_never_run() {
    let f = fixture();
    let ticket = TicketId::new();
    let ws = f
        .git
        .prepare(&f.spec, ticket, &BranchName::for_ticket(ticket))
        .await
        .unwrap();

    // A hook planted in the shared clone, as malicious repo content might try.
    let marker = f._dir.path().join("hook-ran");
    let hooks = f.git.mirror_path(&f.spec.id).join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for hook in ["pre-commit", "commit-msg", "post-commit", "pre-push"] {
        let path = hooks.join(hook);
        std::fs::write(
            &path,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    std::fs::write(ws.path.join("x.txt"), "x\n").unwrap();
    assert!(f.git.commit(&ws, "x", &author()).await.unwrap().is_some());
    f.git.push(&ws).await.unwrap();
    assert!(!marker.exists(), "a repository hook was executed");
    f.git.release(&ws).await.unwrap();
}

#[tokio::test]
async fn unreachable_remote_is_an_upstream_error() {
    let f = fixture();
    let bad = RepoSpec {
        id: RepoId::new("gone").unwrap(),
        remote: f._dir.path().join("nope.git").to_str().unwrap().to_owned(),
        default_branch: BranchName::new("main").unwrap(),
    };
    let ticket = TicketId::new();
    let err = f
        .git
        .prepare(&bad, ticket, &BranchName::for_ticket(ticket))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Upstream { .. }), "{err}");
    // A failed clone leaves no cache behind, so a later attempt starts clean.
    assert!(!f.git.mirror_path(&bad.id).exists());
}
