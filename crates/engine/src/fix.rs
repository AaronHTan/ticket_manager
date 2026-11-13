//! Turning a ticket into a proposed code change.
//!
//! ```text
//! Ticket ─resolve─▶ RepoSpec ─prepare─▶ Workspace ─Fixer─▶ edits
//!        ─commit─▶ push ─CodeHost─▶ change request ─release─▶ (workspace gone)
//! ```
//!
//! [`FixCoordinator`] owns that sequence. It is not yet called from triage:
//! the next stage adds a routing decision that hands suitable tickets to it,
//! together with a [`Fixer`] that actually writes code.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ticket_core::ports::{CodeHost, Fixer, SourceControl};
use ticket_core::{
    BranchName, ChangeRequest, ChangeRequestRef, ClientId, CommitId, Error, FixOutcome, RepoId,
    RepoSpec, Result, Signature, Ticket, Workspace,
};
use tracing::{info, warn};

/// One configured repository, as written under `[repositories.<id>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    /// Clone URL or local path.
    pub remote: String,
    /// Branch work starts from and change requests target.
    #[serde(default = "default_branch")]
    pub default_branch: BranchName,
    /// Clients whose tickets may change this repository. Empty means any client.
    #[serde(default)]
    pub clients: Vec<ClientId>,
    /// Ticket components that point to this repository (case-insensitive).
    #[serde(default)]
    pub components: Vec<String>,
}

fn default_branch() -> BranchName {
    BranchName::new("main").expect("`main` is a valid branch name")
}

impl RepositoryConfig {
    fn allows(&self, client: &ClientId) -> bool {
        self.clients.is_empty() || self.clients.contains(client)
    }

    fn matches_component(&self, components: &[String]) -> bool {
        components.iter().any(|c| {
            self.components
                .iter()
                .any(|own| own.eq_ignore_ascii_case(c.trim()))
        })
    }
}

/// Decides which repository a ticket concerns.
///
/// Pure and synchronous, like [`TicketRouter`](crate::TicketRouter). Rules,
/// in order, considering only repositories the ticket's client may use:
///
/// 1. the repository named in the ticket's [`CodeContext`](ticket_core::CodeContext);
/// 2. the first repository (by id) whose `components` match a ticket component;
/// 3. the client's only repository, if it has exactly one.
///
/// Tickets can name a repository but never a remote, so a ticket cannot make
/// the service clone or push anywhere that is not configured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepositoryDirectory {
    repos: BTreeMap<RepoId, RepositoryConfig>,
}

impl RepositoryDirectory {
    /// Builds the directory, rejecting invalid remotes.
    pub fn new(repos: BTreeMap<RepoId, RepositoryConfig>) -> Result<Self> {
        let directory = Self { repos };
        for id in directory.repos.keys() {
            directory.spec(id).expect("id is present").validate()?;
        }
        Ok(directory)
    }

    /// True if no repositories are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }

    /// Ids of the configured repositories.
    pub fn ids(&self) -> impl Iterator<Item = &RepoId> {
        self.repos.keys()
    }

    /// The spec for a configured repository.
    #[must_use]
    pub fn spec(&self, id: &RepoId) -> Option<RepoSpec> {
        self.repos.get(id).map(|config| RepoSpec {
            id: id.clone(),
            remote: config.remote.clone(),
            default_branch: config.default_branch.clone(),
        })
    }

    /// The repository `ticket` should be fixed in, if one can be determined.
    #[must_use]
    pub fn resolve(&self, ticket: &Ticket) -> Option<RepoSpec> {
        let allowed = |config: &&RepositoryConfig| config.allows(&ticket.client);

        if let Some(hint) = ticket
            .code_context
            .as_ref()
            .and_then(|c| c.repository.as_ref())
        {
            match self.repos.get(hint).filter(allowed) {
                Some(_) => return self.spec(hint),
                None => warn!(
                    ticket = %ticket.id,
                    repository = %hint,
                    "ticket names a repository that is not configured for its client"
                ),
            }
        }

        if let Some((id, _)) = self
            .repos
            .iter()
            .filter(|(_, config)| allowed(config))
            .find(|(_, config)| config.matches_component(&ticket.components))
        {
            return self.spec(id);
        }

        let mut accessible = self.repos.iter().filter(|(_, config)| allowed(config));
        match (accessible.next(), accessible.next()) {
            (Some((id, _)), None) => self.spec(id),
            _ => None,
        }
    }
}

/// What [`FixCoordinator::run`] achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// Runs a [`Fixer`] against a ticket inside a throwaway git workspace.
pub struct FixCoordinator {
    directory: RepositoryDirectory,
    scm: Arc<dyn SourceControl>,
    host: Arc<dyn CodeHost>,
    fixer: Arc<dyn Fixer>,
    author: Signature,
}

impl FixCoordinator {
    /// Creates a coordinator.
    #[must_use]
    pub fn new(
        directory: RepositoryDirectory,
        scm: Arc<dyn SourceControl>,
        host: Arc<dyn CodeHost>,
        fixer: Arc<dyn Fixer>,
        author: Signature,
    ) -> Self {
        Self {
            directory,
            scm,
            host,
            fixer,
            author,
        }
    }

    /// The repositories this coordinator can change.
    #[must_use]
    pub fn directory(&self) -> &RepositoryDirectory {
        &self.directory
    }

    /// Attempts a fix for `ticket`. The workspace is always released, whether
    /// or not the attempt succeeds. Pushed branches stay on the remote.
    pub async fn run(&self, ticket: &Ticket) -> Result<FixResult> {
        let Some(repo) = self.directory.resolve(ticket) else {
            return Ok(FixResult::NoRepository);
        };
        let branch = BranchName::for_ticket(ticket.id);
        let workspace = self.scm.prepare(&repo, ticket.id, &branch).await?;

        let result = self.attempt(ticket, &repo, &workspace).await;
        if let Err(error) = self.scm.release(&workspace).await {
            warn!(
                ticket = %ticket.id,
                path = %workspace.path.display(),
                %error,
                "could not release workspace"
            );
        }
        result
    }

    async fn attempt(
        &self,
        ticket: &Ticket,
        repo: &RepoSpec,
        workspace: &Workspace,
    ) -> Result<FixResult> {
        let FixOutcome { summary } = self.fixer.attempt(ticket, workspace).await?;
        let message = commit_message(ticket, &summary);
        let Some(commit) = self.scm.commit(workspace, &message, &self.author).await? else {
            info!(ticket = %ticket.id, repo = %repo.id, "fixer made no changes");
            return Ok(FixResult::NoChanges {
                repo: repo.id.clone(),
                summary,
            });
        };
        self.scm.push(workspace).await?;
        let change_request = self
            .host
            .open_change_request(&ChangeRequest {
                repo: repo.id.clone(),
                branch: workspace.branch.clone(),
                base: repo.default_branch.clone(),
                title: subject(ticket),
                body: change_request_body(ticket, &summary),
            })
            .await?;
        info!(
            ticket = %ticket.id,
            repo = %repo.id,
            %commit,
            change_request = %change_request.id,
            "fix proposed"
        );
        Ok(FixResult::Proposed {
            repo: repo.id.clone(),
            branch: workspace.branch.clone(),
            commit,
            change_request,
            summary,
        })
    }
}

/// How the ticket is referred to in commits: its external key, else its id.
fn ticket_reference(ticket: &Ticket) -> String {
    ticket
        .external_ref
        .as_ref()
        .map_or_else(|| ticket.id.to_string(), |r| r.key.clone())
}

/// One-line subject, kept within the conventional 72 characters.
fn subject(ticket: &Ticket) -> String {
    const MAX: usize = 72;
    let subject = format!("Fix {}: {}", ticket_reference(ticket), ticket.title);
    if subject.chars().count() <= MAX {
        return subject;
    }
    let mut short: String = subject.chars().take(MAX - 3).collect();
    short.push_str("...");
    short
}

fn commit_message(ticket: &Ticket, summary: &str) -> String {
    format!(
        "{}\n\n{}\n\nTicket: {}",
        subject(ticket),
        summary.trim(),
        ticket_reference(ticket)
    )
}

fn change_request_body(ticket: &Ticket, summary: &str) -> String {
    let mut body = format!(
        "Automated change for ticket {} ({}).\n\n{}\n",
        ticket_reference(ticket),
        ticket.id,
        summary.trim()
    );
    for link in &ticket.links {
        body.push_str(&format!("\n- {}", link.url));
    }
    body
}

/// A [`Fixer`] that never changes anything. The default until a real one is configured.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopFixer;

#[async_trait]
impl Fixer for NoopFixer {
    async fn attempt(&self, _ticket: &Ticket, _workspace: &Workspace) -> Result<FixOutcome> {
        Ok(FixOutcome {
            summary: "No automated fixer is configured.".into(),
        })
    }
}

/// A [`CodeHost`] that logs instead of opening a pull request. Stands in for
/// GitHub or GitLab. Reviewers find the change by its branch.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogCodeHost;

#[async_trait]
impl CodeHost for LogCodeHost {
    async fn open_change_request(&self, request: &ChangeRequest) -> Result<ChangeRequestRef> {
        if request.branch == request.base {
            return Err(Error::Validation(
                "a change request needs a branch other than its base".into(),
            ));
        }
        info!(
            repo = %request.repo,
            branch = %request.branch,
            base = %request.base,
            title = %request.title,
            "change request ready for review"
        );
        Ok(ChangeRequestRef {
            id: request.branch.to_string(),
            url: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ticket_core::{CodeContext, ExternalRef, NewTicket};

    use super::*;

    fn repo(remote: &str, clients: &[&str], components: &[&str]) -> RepositoryConfig {
        RepositoryConfig {
            remote: remote.into(),
            default_branch: default_branch(),
            clients: clients.iter().map(|c| ClientId::new(*c).unwrap()).collect(),
            components: components.iter().map(|c| (*c).to_owned()).collect(),
        }
    }

    fn directory(repos: &[(&str, RepositoryConfig)]) -> RepositoryDirectory {
        RepositoryDirectory::new(
            repos
                .iter()
                .map(|(id, config)| (RepoId::new(*id).unwrap(), config.clone()))
                .collect(),
        )
        .unwrap()
    }

    fn ticket(client: &str, components: &[&str], hint: Option<&str>) -> Ticket {
        let mut new = NewTicket::new(ClientId::new(client).unwrap(), "Checkout 500", "")
            .with_components(components.iter().copied());
        if let Some(hint) = hint {
            new = new.with_code_context(CodeContext {
                repository: Some(RepoId::new(hint).unwrap()),
                ..CodeContext::default()
            });
        }
        Ticket::receive(new, Utc::now()).unwrap()
    }

    fn resolved(dir: &RepositoryDirectory, t: &Ticket) -> Option<String> {
        dir.resolve(t).map(|spec| spec.id.to_string())
    }

    #[test]
    fn explicit_hint_wins_when_allowed() {
        let dir = directory(&[
            ("api", repo("git@x:acme/api.git", &["acme"], &["checkout"])),
            ("web", repo("git@x:acme/web.git", &["acme"], &[])),
        ]);
        let t = ticket("acme", &["checkout"], Some("web"));
        assert_eq!(resolved(&dir, &t).as_deref(), Some("web"));
    }

    #[test]
    fn hint_for_another_clients_repository_is_ignored() {
        let dir = directory(&[
            ("acme-web", repo("git@x:acme/web.git", &["acme"], &[])),
            ("globex-web", repo("git@x:globex/web.git", &["globex"], &[])),
        ]);
        let t = ticket("acme", &[], Some("globex-web"));
        // Falls through to the client's only repository.
        assert_eq!(resolved(&dir, &t).as_deref(), Some("acme-web"));
        // An unknown client gets nothing.
        assert_eq!(
            resolved(&dir, &ticket("initech", &[], Some("globex-web"))),
            None
        );
    }

    #[test]
    fn components_match_case_insensitively() {
        let dir = directory(&[
            ("api", repo("git@x:acme/api.git", &[], &["Payments"])),
            (
                "web",
                repo("git@x:acme/web.git", &[], &["frontend", "checkout"]),
            ),
        ]);
        assert_eq!(
            resolved(&dir, &ticket("acme", &["CHECKOUT"], None)).as_deref(),
            Some("web")
        );
        assert_eq!(
            resolved(&dir, &ticket("acme", &["payments"], None)).as_deref(),
            Some("api")
        );
        // Two accessible repositories and no match: ambiguous.
        assert_eq!(resolved(&dir, &ticket("acme", &["docs"], None)), None);
    }

    #[test]
    fn sole_accessible_repository_is_the_fallback() {
        let dir = directory(&[("web", repo("/srv/git/web.git", &[], &[]))]);
        assert_eq!(
            resolved(&dir, &ticket("anyone", &[], None)).as_deref(),
            Some("web")
        );
        assert_eq!(
            resolved(&RepositoryDirectory::default(), &ticket("a", &[], None)),
            None
        );
    }

    #[test]
    fn invalid_remotes_are_rejected_at_start_up() {
        let repos = BTreeMap::from([(
            RepoId::new("web").unwrap(),
            repo("--upload-pack=evil", &[], &[]),
        )]);
        assert!(matches!(
            RepositoryDirectory::new(repos),
            Err(Error::Config(_))
        ));
    }

    #[test]
    fn repository_config_defaults_to_main() {
        let config: RepositoryConfig =
            serde_json::from_str(r#"{"remote":"git@x:acme/web.git"}"#).unwrap();
        assert_eq!(config.default_branch.as_str(), "main");
        assert!(config.clients.is_empty());
    }

    #[test]
    fn commit_messages_reference_the_ticket() {
        let mut new = NewTicket::new(ClientId::new("acme").unwrap(), "x".repeat(100), "")
            .with_external_ref(ExternalRef::new("jira-acme", "OPS-9"));
        new.title = format!("Checkout fails {}", "x".repeat(100));
        let t = Ticket::receive(new, Utc::now()).unwrap();
        let s = subject(&t);
        assert!(s.starts_with("Fix OPS-9: Checkout fails"));
        assert_eq!(s.chars().count(), 72);
        let message = commit_message(&t, "  Guard against empty cart.  ");
        assert!(message.ends_with("Guard against empty cart.\n\nTicket: OPS-9"));
    }
}
