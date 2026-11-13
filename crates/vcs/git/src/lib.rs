//! A [`SourceControl`] adapter that drives the system `git` binary.
//!
//! Layout on disk:
//!
//! ```text
//! cache_dir/<repo-id>.git        bare clone, shared by every ticket for that repo
//! workspace_dir/<ticket-id>/     one linked worktree per ticket, on branch ticket/<id>
//! ```
//!
//! The bare clone is fetched before each new workspace, so work always
//! starts from the remote's current default branch. Worktrees share the
//! clone's object store, so a new workspace costs a checkout, not a clone.
//!
//! Shelling out (rather than linking libgit2 or gitoxide) means every
//! transport and credential setup that works for `git` on the host (SSH
//! agent, credential helpers, `insteadOf` rewrites) works here too.
//!
//! # Hardening
//!
//! Ticket content eventually ends up in these working trees, so:
//!
//! * arguments are passed as a vector, never through a shell, and every
//!   caller-supplied value is a validated [`RepoId`], [`BranchName`] or path
//!   we built ourselves;
//! * repository hooks and fsmonitor are disabled (`core.hooksPath=/dev/null`,
//!   `core.fsmonitor=false`), and commits and pushes use `--no-verify`;
//! * external diff and textconv drivers are disabled for diffs;
//! * git never prompts (`GIT_TERMINAL_PROMPT=0`, stdin closed), and every
//!   command runs under a timeout and is killed when it expires.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use ticket_core::ports::SourceControl;
use ticket_core::{
    BranchName, CommitId, Error, RepoId, RepoSpec, Result, Signature, TicketId, Workspace,
};
use tokio::process::Command;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::{debug, info, warn};

/// System name used in [`Error::Upstream`].
const SYSTEM: &str = "git";
/// Upper bound on stderr quoted in errors.
const MAX_STDERR: usize = 2048;

/// Settings for [`GitCli`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitConfig {
    /// The `git` executable, looked up on `PATH` if not absolute.
    pub binary: PathBuf,
    /// Where bare clones are kept.
    pub cache_dir: PathBuf,
    /// Where per-ticket working trees are created.
    pub workspace_dir: PathBuf,
    /// Limit for any single git command, in seconds. Covers the initial clone.
    pub command_timeout_secs: u64,
    /// Name used for commits made by the service.
    pub author_name: String,
    /// Email used for commits made by the service.
    pub author_email: String,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("git"),
            cache_dir: PathBuf::from("data/repos"),
            workspace_dir: PathBuf::from("data/workspaces"),
            command_timeout_secs: 300,
            author_name: "ticket-handler".into(),
            author_email: "ticket-handler@localhost".into(),
        }
    }
}

impl GitConfig {
    /// The identity commits are made with.
    #[must_use]
    pub fn author(&self) -> Signature {
        Signature {
            name: self.author_name.clone(),
            email: self.author_email.clone(),
        }
    }
}

/// [`SourceControl`] backed by the `git` command-line client.
#[derive(Debug)]
pub struct GitCli {
    binary: PathBuf,
    cache_dir: PathBuf,
    workspace_dir: PathBuf,
    timeout: Duration,
    /// One lock per repository. Fetches, worktree changes and pushes all
    /// touch the shared bare clone, and git's own lock files would make
    /// concurrent operations fail rather than wait.
    locks: StdMutex<HashMap<RepoId, Arc<Mutex<()>>>>,
}

impl GitCli {
    /// Resolves the configured directories to absolute paths. Nothing is
    /// created on disk until a workspace is first prepared.
    pub fn new(config: &GitConfig) -> Result<Self> {
        let absolute = |dir: &Path| -> Result<PathBuf> {
            std::path::absolute(dir)
                .map_err(|e| Error::Config(format!("git directory {}: {e}", dir.display())))
        };
        Ok(Self {
            binary: config.binary.clone(),
            cache_dir: absolute(&config.cache_dir)?,
            workspace_dir: absolute(&config.workspace_dir)?,
            timeout: Duration::from_secs(config.command_timeout_secs.max(1)),
            locks: StdMutex::new(HashMap::new()),
        })
    }

    /// Runs `git --version`. Used at start-up to fail fast when git is missing.
    pub async fn version(&self) -> Result<String> {
        let cwd = std::env::temp_dir();
        let out = self.run(&cwd, ["--version"], &[], "git --version").await?;
        Ok(out.trim().to_owned())
    }

    /// Path of the bare clone for `repo`.
    #[must_use]
    pub fn mirror_path(&self, repo: &RepoId) -> PathBuf {
        self.cache_dir.join(format!("{repo}.git"))
    }

    /// Path of the working tree for `ticket`.
    #[must_use]
    pub fn workspace_path(&self, ticket: TicketId) -> PathBuf {
        self.workspace_dir.join(ticket.to_string())
    }

    async fn lock(&self, repo: &RepoId) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .locks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(locks.entry(repo.clone()).or_default())
        };
        lock.lock_owned().await
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.current_dir(cwd)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    /// Runs a git command and returns its raw output, whatever the exit status.
    async fn output<I, S>(
        &self,
        cwd: &Path,
        args: I,
        envs: &[(&str, &str)],
        what: &str,
    ) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = self.command(cwd);
        cmd.args(args).envs(envs.iter().copied());
        debug!(cwd = %cwd.display(), what, "git");
        match tokio::time::timeout(self.timeout, cmd.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(Error::upstream(
                SYSTEM,
                format!("{what}: could not run {}: {e}", self.binary.display()),
            )),
            Err(_) => Err(Error::upstream(
                SYSTEM,
                format!("{what}: timed out after {}s", self.timeout.as_secs()),
            )),
        }
    }

    /// Runs a git command that must succeed and returns its stdout.
    async fn run<I, S>(
        &self,
        cwd: &Path,
        args: I,
        envs: &[(&str, &str)],
        what: &str,
    ) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.output(cwd, args, envs, what).await?;
        if !output.status.success() {
            return Err(failure(what, &output));
        }
        String::from_utf8(output.stdout)
            .map_err(|e| Error::upstream(SYSTEM, format!("{what}: non-UTF-8 output: {e}")))
    }

    /// True if `git rev-parse --verify` resolves `rev` in `cwd`.
    async fn resolves(&self, cwd: &Path, rev: &str) -> Result<bool> {
        let output = self
            .output(
                cwd,
                ["rev-parse", "--verify", "--quiet", rev],
                &[],
                "rev-parse",
            )
            .await?;
        Ok(output.status.success())
    }

    async fn head(&self, cwd: &Path) -> Result<CommitId> {
        CommitId::new(
            self.run(cwd, ["rev-parse", "HEAD"], &[], "rev-parse HEAD")
                .await?,
        )
    }

    /// Clones `repo` if needed, points it at the configured remote and fetches.
    async fn sync_mirror(&self, repo: &RepoSpec) -> Result<PathBuf> {
        for dir in [&self.cache_dir, &self.workspace_dir] {
            tokio::fs::create_dir_all(dir).await.map_err(|e| {
                Error::Config(format!("creating git directory {}: {e}", dir.display()))
            })?;
        }
        let mirror = self.mirror_path(&repo.id);
        if !mirror.exists() {
            // Clone next to the final path and rename, so an interrupted clone
            // never leaves a half-initialised cache behind.
            let partial = self.cache_dir.join(format!(".{}.git.partial", repo.id));
            if partial.exists() {
                remove_dir(&partial).await?;
            }
            info!(repo = %repo.id, "cloning repository");
            self.run(
                &self.cache_dir,
                [
                    OsStr::new("clone"),
                    OsStr::new("--bare"),
                    OsStr::new("--no-tags"),
                    OsStr::new("--quiet"),
                    OsStr::new("--"),
                    OsStr::new(&repo.remote),
                    partial.as_os_str(),
                ],
                &[],
                "clone",
            )
            .await?;
            self.run(
                &partial,
                [
                    "config",
                    "remote.origin.fetch",
                    "+refs/heads/*:refs/remotes/origin/*",
                ],
                &[],
                "config fetch refspec",
            )
            .await?;
            tokio::fs::rename(&partial, &mirror)
                .await
                .map_err(|e| Error::upstream(SYSTEM, format!("finishing clone: {e}")))?;
        }
        // Follow configuration changes to the remote URL.
        self.run(
            &mirror,
            ["config", "remote.origin.url", repo.remote.as_str()],
            &[],
            "config remote url",
        )
        .await?;
        self.run(
            &mirror,
            ["fetch", "--quiet", "--prune", "--no-tags", "origin"],
            &[],
            "fetch",
        )
        .await?;
        Ok(mirror)
    }
}

#[async_trait]
impl SourceControl for GitCli {
    async fn prepare(
        &self,
        repo: &RepoSpec,
        ticket: TicketId,
        branch: &BranchName,
    ) -> Result<Workspace> {
        repo.validate()?;
        let _guard = self.lock(&repo.id).await;
        let mirror = self.sync_mirror(repo).await?;

        let path = self.workspace_path(ticket);
        if path.exists() {
            return Err(Error::Conflict(format!(
                "workspace for ticket {ticket} already exists"
            )));
        }
        let base_ref = format!("refs/remotes/origin/{}", repo.default_branch);
        if !self
            .resolves(&mirror, &format!("{base_ref}^{{commit}}"))
            .await?
        {
            return Err(Error::NotFound(format!(
                "branch `{}` in repository `{}`",
                repo.default_branch, repo.id
            )));
        }
        if self
            .resolves(&mirror, &format!("refs/heads/{branch}"))
            .await?
        {
            return Err(Error::Conflict(format!(
                "branch `{branch}` already exists in repository `{}`",
                repo.id
            )));
        }

        self.run(
            &mirror,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--quiet"),
                OsStr::new("-b"),
                OsStr::new(branch.as_str()),
                OsStr::new("--"),
                path.as_os_str(),
                OsStr::new(&base_ref),
            ],
            &[],
            "worktree add",
        )
        .await?;
        let base_commit = self.head(&path).await?;
        info!(repo = %repo.id, %ticket, %branch, base = %base_commit, "workspace ready");
        Ok(Workspace {
            ticket_id: ticket,
            repo: repo.id.clone(),
            path,
            branch: branch.clone(),
            base_commit,
        })
    }

    async fn diff(&self, workspace: &Workspace) -> Result<String> {
        // Staging makes new files part of the diff. `commit` stages everything
        // anyway, so this changes nothing that matters.
        self.run(&workspace.path, ["add", "--all"], &[], "add")
            .await?;
        self.run(
            &workspace.path,
            [
                "diff",
                "--cached",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                workspace.base_commit.as_str(),
                "--",
            ],
            &[],
            "diff",
        )
        .await
    }

    async fn commit(
        &self,
        workspace: &Workspace,
        message: &str,
        author: &Signature,
    ) -> Result<Option<CommitId>> {
        if message.trim().is_empty() {
            return Err(Error::Validation("commit message must not be empty".into()));
        }
        self.run(&workspace.path, ["add", "--all"], &[], "add")
            .await?;
        let staged = self
            .output(
                &workspace.path,
                ["diff", "--cached", "--quiet"],
                &[],
                "diff --cached",
            )
            .await?;
        match staged.status.code() {
            Some(0) => return Ok(None),
            Some(1) => {}
            _ => return Err(failure("diff --cached", &staged)),
        }
        let identity = [
            ("GIT_AUTHOR_NAME", author.name.as_str()),
            ("GIT_AUTHOR_EMAIL", author.email.as_str()),
            ("GIT_COMMITTER_NAME", author.name.as_str()),
            ("GIT_COMMITTER_EMAIL", author.email.as_str()),
        ];
        self.run(
            &workspace.path,
            [
                "commit",
                "--quiet",
                "--no-verify",
                "--no-gpg-sign",
                "-m",
                message,
            ],
            &identity,
            "commit",
        )
        .await?;
        self.head(&workspace.path).await.map(Some)
    }

    async fn push(&self, workspace: &Workspace) -> Result<()> {
        let _guard = self.lock(&workspace.repo).await;
        let refspec = format!("refs/heads/{0}:refs/heads/{0}", workspace.branch);
        self.run(
            &workspace.path,
            ["push", "--quiet", "--no-verify", "--", "origin", &refspec],
            &[],
            "push",
        )
        .await?;
        info!(repo = %workspace.repo, branch = %workspace.branch, "pushed");
        Ok(())
    }

    async fn release(&self, workspace: &Workspace) -> Result<()> {
        let _guard = self.lock(&workspace.repo).await;
        let mirror = self.mirror_path(&workspace.repo);
        let removed = self
            .output(
                &mirror,
                [
                    OsStr::new("worktree"),
                    OsStr::new("remove"),
                    OsStr::new("--force"),
                    OsStr::new("--"),
                    workspace.path.as_os_str(),
                ],
                &[],
                "worktree remove",
            )
            .await?;
        if !removed.status.success() {
            warn!(path = %workspace.path.display(), "git could not remove worktree; deleting it");
        }
        if workspace.path.exists() {
            remove_dir(&workspace.path).await?;
        }
        self.run(&mirror, ["worktree", "prune"], &[], "worktree prune")
            .await?;
        let branch_ref = format!("refs/heads/{}", workspace.branch);
        if self.resolves(&mirror, &branch_ref).await? {
            self.run(
                &mirror,
                ["update-ref", "-d", &branch_ref],
                &[],
                "update-ref -d",
            )
            .await?;
        }
        Ok(())
    }
}

fn failure(what: &str, output: &Output) -> Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut stderr = stderr.trim().to_owned();
    if stderr.len() > MAX_STDERR {
        let mut end = MAX_STDERR;
        while !stderr.is_char_boundary(end) {
            end -= 1;
        }
        stderr.truncate(end);
        stderr.push_str("...");
    }
    Error::upstream(
        SYSTEM,
        format!("{what} failed ({}): {stderr}", output.status),
    )
}

async fn remove_dir(path: &Path) -> Result<()> {
    tokio::fs::remove_dir_all(path)
        .await
        .map_err(|e| Error::upstream(SYSTEM, format!("removing {}: {e}", path.display())))
}
