use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Result, TicketId};

/// Names a source repository known to the handler, such as `"web-app"`.
///
/// This is a configuration key, not a URL. The remote it points to is
/// looked up in the repository directory, so tickets can never make the
/// service clone arbitrary locations.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoId(String);

impl RepoId {
    /// Maximum length of a repository id.
    pub const MAX_LEN: usize = 128;

    /// Creates a repository id. Allowed characters: ASCII alphanumerics, `-`, `_`, `.`.
    /// It may not start with `.` or `-`.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let trimmed = value.trim();
        let valid_chars = trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if trimmed.is_empty()
            || trimmed.len() > Self::MAX_LEN
            || !valid_chars
            || trimmed.starts_with(['.', '-'])
        {
            return Err(Error::Validation(format!(
                "repository id `{trimmed}` must be 1-{} characters of [A-Za-z0-9._-], \
                 not starting with `.` or `-`",
                Self::MAX_LEN
            )));
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Returns the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RepoId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<RepoId> for String {
    fn from(value: RepoId) -> Self {
        value.0
    }
}

impl FromStr for RepoId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Hints from the reporter about which code a ticket concerns.
///
/// Every field is optional and none is trusted: the repository must still
/// resolve through configuration, and paths are only ever read relative to a
/// checked-out workspace.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CodeContext {
    /// The repository the problem is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepoId>,
    /// Branch, tag or commit where the problem was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// Repository-relative paths thought to be involved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Released version where the problem was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl CodeContext {
    /// Maximum number of paths.
    pub const MAX_PATHS: usize = 64;
    /// Maximum length of a path, ref or version, in bytes.
    pub const MAX_FIELD_LEN: usize = 512;

    /// Checks sizes and rejects refs and paths that could escape a workspace
    /// or be read as command-line options.
    pub fn validate(&self) -> Result<()> {
        if self.paths.len() > Self::MAX_PATHS {
            return Err(Error::Validation(format!(
                "code context allows at most {} paths",
                Self::MAX_PATHS
            )));
        }
        for path in &self.paths {
            validate_relative_path(path)?;
        }
        if let Some(git_ref) = &self.git_ref {
            check_len("git ref", git_ref, Self::MAX_FIELD_LEN)?;
            if git_ref.starts_with('-') || git_ref.chars().any(|c| c.is_control() || c == ' ') {
                return Err(Error::Validation(format!("invalid git ref `{git_ref}`")));
            }
        }
        if let Some(version) = &self.version {
            check_len("version", version, Self::MAX_FIELD_LEN)?;
        }
        Ok(())
    }

    /// True if no hint is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// A repository the handler may change: where it lives and which branch to start from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSpec {
    /// Configuration key.
    pub id: RepoId,
    /// Clone URL or local path, as understood by `git clone`.
    pub remote: String,
    /// Branch that work starts from and change requests target.
    pub default_branch: BranchName,
}

impl RepoSpec {
    /// Checks that the remote cannot be read as a command-line option.
    pub fn validate(&self) -> Result<()> {
        let remote = self.remote.trim();
        if remote.is_empty()
            || remote.starts_with('-')
            || remote.chars().any(char::is_control)
            || remote.len() > 2048
        {
            return Err(Error::Config(format!(
                "repository `{}` has an invalid remote",
                self.id
            )));
        }
        Ok(())
    }
}

/// A git branch name that is safe to pass to `git` as an argument.
///
/// Stricter than `git check-ref-format`: only `[A-Za-z0-9._/-]`, no empty or
/// dot-prefixed path components, no `..`, no `.lock` suffix, and never a
/// leading `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BranchName(String);

impl BranchName {
    /// Maximum length of a branch name.
    pub const MAX_LEN: usize = 200;

    /// Validates a branch name.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let valid_chars = value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
        let valid_components = value
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"));
        if value.is_empty()
            || value.len() > Self::MAX_LEN
            || !valid_chars
            || !valid_components
            || value.starts_with('-')
            || value.ends_with('.')
            || value.contains("..")
        {
            return Err(Error::Validation(format!("invalid branch name `{value}`")));
        }
        Ok(Self(value))
    }

    /// The working branch for a ticket: `ticket/<ticket-id>`.
    #[must_use]
    pub fn for_ticket(id: TicketId) -> Self {
        Self(format!("ticket/{id}"))
    }

    /// Returns the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BranchName {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<BranchName> for String {
    fn from(value: BranchName) -> Self {
        value.0
    }
}

impl FromStr for BranchName {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl fmt::Display for BranchName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A full or abbreviated commit hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommitId(String);

impl CommitId {
    /// Validates a hex commit hash (SHA-1 or SHA-256, possibly abbreviated).
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into().trim().to_ascii_lowercase();
        if !(7..=64).contains(&value.len()) || !value.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Validation(format!("invalid commit id `{value}`")));
        }
        Ok(Self(value))
    }

    /// Returns the hash as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CommitId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<CommitId> for String {
    fn from(value: CommitId) -> Self {
        value.0
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A private checkout of a repository on a fresh branch, owned by one ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Ticket the workspace was created for.
    pub ticket_id: TicketId,
    /// Repository checked out.
    pub repo: RepoId,
    /// Absolute path of the working tree.
    pub path: PathBuf,
    /// Branch checked out in the working tree.
    pub branch: BranchName,
    /// Commit the branch started from.
    pub base_commit: CommitId,
}

/// Commit author and committer identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// Name.
    pub name: String,
    /// Email.
    pub email: String,
}

/// A proposed change for review on a code host (a pull or merge request).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRequest {
    /// Repository the change is in.
    pub repo: RepoId,
    /// Branch holding the change.
    pub branch: BranchName,
    /// Branch the change should merge into.
    pub base: BranchName,
    /// One-line title.
    pub title: String,
    /// Description for reviewers.
    pub body: String,
}

/// Where an opened change request can be found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRequestRef {
    /// Host-specific identifier, such as a pull request number.
    pub id: String,
    /// Web URL, when the host has one.
    pub url: Option<String>,
}

/// What a [`Fixer`](crate::ports::Fixer) did in a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixOutcome {
    /// Explanation of the change (or why none was made), used in the commit
    /// message and the change request.
    pub summary: String,
}

/// What an attempt to fix a ticket in code achieved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum FixResult {
    /// No configured repository matches the ticket.
    NoRepository,
    /// The fixer ran but changed nothing.
    NoChanges {
        /// Repository that was examined.
        repo: RepoId,
        /// The fixer's explanation.
        summary: String,
    },
    /// A change was committed, pushed and proposed for review.
    Proposed {
        /// Repository changed.
        repo: RepoId,
        /// Branch holding the change.
        branch: BranchName,
        /// The commit.
        commit: CommitId,
        /// Where reviewers find it.
        change_request: ChangeRequestRef,
        /// The fixer's explanation.
        summary: String,
    },
    /// The attempt failed (git, fixer or code host error).
    Failed {
        /// What went wrong.
        error: String,
    },
}

impl FixResult {
    /// Stable short name: `no_repository`, `no_changes`, `proposed` or `failed`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::NoRepository => "no_repository",
            Self::NoChanges { .. } => "no_changes",
            Self::Proposed { .. } => "proposed",
            Self::Failed { .. } => "failed",
        }
    }
}

/// A recorded fix attempt for a ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixAttempt {
    /// Ticket the attempt was for.
    pub ticket_id: TicketId,
    /// What happened.
    #[serde(flatten)]
    pub result: FixResult,
    /// When the attempt finished.
    pub attempted_at: DateTime<Utc>,
}

fn validate_relative_path(path: &str) -> Result<()> {
    check_len("path", path, CodeContext::MAX_FIELD_LEN)?;
    let escapes = path.is_empty()
        || path.starts_with(['/', '\\', '-'])
        || path.contains(':')
        || path.chars().any(char::is_control)
        || path.split(['/', '\\']).any(|part| part == "..");
    if escapes {
        return Err(Error::Validation(format!(
            "path `{path}` must be relative to the repository root and stay inside it"
        )));
    }
    Ok(())
}

pub(crate) fn check_len(what: &str, value: &str, max: usize) -> Result<()> {
    if value.len() > max {
        return Err(Error::Validation(format!("{what} exceeds {max} bytes")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_rejects_invalid_values() {
        assert!(RepoId::new("web-app_2.0").is_ok());
        assert!(RepoId::new("").is_err());
        assert!(RepoId::new("../etc").is_err());
        assert!(RepoId::new("a/b").is_err());
        assert!(RepoId::new(".hidden").is_err());
        assert!(RepoId::new("-rf").is_err());
        assert!(RepoId::new("r".repeat(129)).is_err());
        assert!(serde_json::from_str::<RepoId>("\"bad id\"").is_err());
    }

    #[test]
    fn code_context_rejects_escaping_paths_and_option_like_refs() {
        let ok = CodeContext {
            repository: Some(RepoId::new("web-app").unwrap()),
            git_ref: Some("release/1.2".into()),
            paths: vec!["src/login.rs".into(), "docs/a..b.md".into()],
            version: Some("1.2.3".into()),
        };
        assert!(ok.validate().is_ok());

        for path in ["../secret", "src/../../x", "/etc/passwd", "C:\\x", "-x", ""] {
            let ctx = CodeContext {
                paths: vec![path.into()],
                ..CodeContext::default()
            };
            assert!(ctx.validate().is_err(), "{path} should be rejected");
        }
        for git_ref in ["--upload-pack=evil", "a b", "a\nb"] {
            let ctx = CodeContext {
                git_ref: Some(git_ref.into()),
                ..CodeContext::default()
            };
            assert!(ctx.validate().is_err(), "{git_ref} should be rejected");
        }
        let too_many = CodeContext {
            paths: vec!["a".into(); CodeContext::MAX_PATHS + 1],
            ..CodeContext::default()
        };
        assert!(too_many.validate().is_err());
    }

    #[test]
    fn branch_names_are_safe_git_arguments() {
        for ok in ["main", "release/1.2", "ticket/0190-ab_c", "feature/x.y"] {
            assert!(BranchName::new(ok).is_ok(), "{ok} should be accepted");
        }
        for bad in [
            "",
            "-delete",
            "a..b",
            "a/.hidden",
            "a//b",
            "a/",
            "/a",
            "x.lock",
            "x.",
            "a b",
            "a~1",
            "a^",
            "a:b",
            "@{u}",
            "a\\b",
        ] {
            assert!(BranchName::new(bad).is_err(), "{bad} should be rejected");
        }
        let id = TicketId::new();
        let branch = BranchName::for_ticket(id);
        assert_eq!(BranchName::new(branch.as_str()).unwrap(), branch);
    }

    #[test]
    fn commit_ids_are_hex() {
        assert_eq!(CommitId::new("ABCDEF1").unwrap().as_str(), "abcdef1");
        assert!(CommitId::new("abc").is_err());
        assert!(CommitId::new("zzzzzzzz").is_err());
        assert!(CommitId::new("a".repeat(65)).is_err());
    }

    #[test]
    fn repo_spec_rejects_option_like_remotes() {
        let spec = |remote: &str| RepoSpec {
            id: RepoId::new("web-app").unwrap(),
            remote: remote.into(),
            default_branch: BranchName::new("main").unwrap(),
        };
        assert!(spec("git@github.com:acme/web-app.git").validate().is_ok());
        assert!(spec("/srv/git/web-app.git").validate().is_ok());
        assert!(spec("--upload-pack=touch /tmp/x").validate().is_err());
        assert!(spec(" ").validate().is_err());
    }

    #[test]
    fn fix_attempts_serialise_with_a_result_tag() {
        let attempt = FixAttempt {
            ticket_id: TicketId::new(),
            result: FixResult::Proposed {
                repo: RepoId::new("web-app").unwrap(),
                branch: BranchName::new("ticket/x").unwrap(),
                commit: CommitId::new("abcdef1").unwrap(),
                change_request: ChangeRequestRef {
                    id: "ticket/x".into(),
                    url: None,
                },
                summary: "guarded".into(),
            },
            attempted_at: Utc::now(),
        };
        let json = serde_json::to_value(&attempt).unwrap();
        assert_eq!(json["result"], "proposed");
        assert_eq!(json["branch"], "ticket/x");
        let back: FixAttempt = serde_json::from_value(json).unwrap();
        assert_eq!(back, attempt);
        assert_eq!(FixResult::Failed { error: "x".into() }.kind(), "failed");
    }

    #[test]
    fn empty_code_context_serialises_compactly() {
        assert!(CodeContext::default().is_empty());
        assert_eq!(
            serde_json::to_string(&CodeContext::default()).unwrap(),
            "{}"
        );
    }
}
