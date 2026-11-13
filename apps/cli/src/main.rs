//! `ticket`: a standalone command-line client for the ticket-handler service.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde::Serialize;
use ticket_client::Client;
use ticket_client::api::{
    Category, ClientId, CodeContext, HistoricalTicket, ImportHistoryRequest, Link,
    ListTicketsQuery, ListedTicket, RecordResolutionRequest, RepoId, Reporter, Severity,
    SubmitTicketRequest, TicketId, TicketKind, TicketStatus,
};

#[derive(Debug, Parser)]
#[command(
    name = "ticket",
    version,
    about = "Submit and inspect tickets on a ticket-handler server"
)]
struct Cli {
    /// Server base URL.
    #[arg(
        long,
        env = "TICKET_HANDLER_URL",
        default_value = "http://127.0.0.1:8080",
        global = true
    )]
    server: String,

    /// API bearer token.
    #[arg(
        long,
        env = "TICKET_HANDLER_TOKEN",
        hide_env_values = true,
        global = true
    )]
    token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check server health.
    Health,
    /// Submit a new ticket for triage.
    Submit {
        /// Client (tenant) id.
        #[arg(long)]
        client: ClientId,
        /// Short summary.
        #[arg(long)]
        title: String,
        /// Full description.
        #[arg(long, default_value = "")]
        description: String,
        /// Reporter's severity estimate (low, medium, high, critical).
        #[arg(long)]
        severity: Option<Severity>,
        /// Label to attach (repeatable).
        #[arg(long = "label")]
        labels: Vec<String>,
        /// Reporter name or email.
        #[arg(long)]
        reporter: Option<String>,
        /// Ticket kind (bug, incident, feature_request, task, question, other).
        #[arg(long, default_value = "other")]
        kind: TicketKind,
        /// Affected component (repeatable).
        #[arg(long = "component")]
        components: Vec<String>,
        /// Configured repository id the problem is in.
        #[arg(long)]
        repo: Option<RepoId>,
        /// Related http(s) link, such as logs or a screenshot (repeatable).
        #[arg(long = "link")]
        links: Vec<String>,
    },
    /// List tickets, newest first.
    List {
        /// Only tickets in this status (received, forwarded, escalated, ...).
        #[arg(long)]
        status: Option<TicketStatus>,
        /// Only tickets of this client.
        #[arg(long)]
        client: Option<ClientId>,
        /// Maximum number of tickets (server caps at 500).
        #[arg(long)]
        limit: Option<u32>,
        /// Print the raw JSON response instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show a ticket and its latest triage report.
    Get {
        /// Ticket id.
        id: TicketId,
    },
    /// Record how a ticket was resolved, teaching the knowledge base.
    Resolve {
        /// Ticket id.
        id: TicketId,
        /// What fixed it.
        #[arg(long)]
        summary: String,
        /// Allow this fix to be applied automatically to future matches.
        #[arg(long)]
        automatable: bool,
        /// Who resolved it.
        #[arg(long)]
        resolved_by: Option<String>,
        /// File holding the fix as a unified diff (for example `git diff > fix.patch`).
        #[arg(long)]
        patch_file: Option<PathBuf>,
        /// Corrected category.
        #[arg(long)]
        category: Option<Category>,
        /// Corrected severity.
        #[arg(long)]
        severity: Option<Severity>,
    },
    /// Import resolved historical tickets from a JSON array file.
    Import {
        /// Path to a JSON file containing an array of historical tickets.
        file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut client = Client::new(&cli.server)?;
    if let Some(token) = cli.token {
        client = client.with_token(token);
    }

    match cli.command {
        Command::Health => print(&client.health().await?),
        Command::Submit {
            client: client_id,
            title,
            description,
            severity,
            labels,
            reporter,
            kind,
            components,
            repo,
            links,
        } => {
            let mut request = SubmitTicketRequest::new(client_id, title, description);
            request.reporter = reporter.map(Reporter::from_label).transpose()?;
            request.kind = kind;
            request.labels = labels;
            request.components = components;
            request.severity = severity;
            request.code_context = repo.map(|repository| CodeContext {
                repository: Some(repository),
                ..CodeContext::default()
            });
            request.links = links.into_iter().map(Link::new).collect();
            print(&client.submit_ticket(&request).await?)
        }
        Command::List {
            status,
            client: client_id,
            limit,
            json,
        } => {
            let query = ListTicketsQuery {
                status,
                client: client_id,
                limit,
            };
            let response = client.list_tickets(&query).await?;
            if json {
                print(&response)
            } else {
                print_table(&response.tickets);
                Ok(())
            }
        }
        Command::Get { id } => print(&client.get_ticket(id).await?),
        Command::Resolve {
            id,
            summary,
            automatable,
            resolved_by,
            patch_file,
            category,
            severity,
        } => {
            let patch = patch_file
                .map(|path| {
                    std::fs::read_to_string(&path)
                        .with_context(|| format!("reading {}", path.display()))
                })
                .transpose()?;
            let request = RecordResolutionRequest {
                summary,
                automatable,
                resolved_by,
                patch,
                category,
                severity,
            };
            print(&client.record_resolution(id, &request).await?)
        }
        Command::Import { file } => {
            let raw = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let tickets: Vec<HistoricalTicket> = serde_json::from_str(&raw)
                .with_context(|| format!("parsing {}", file.display()))?;
            print(
                &client
                    .import_history(&ImportHistoryRequest { tickets })
                    .await?,
            )
        }
    }
}

fn print_table(tickets: &[ListedTicket]) {
    println!(
        "{:<36}  {:<13}  {:<15}  {:<10}  title",
        "id", "status", "kind", "fix"
    );
    for ListedTicket { ticket: t, fix } in tickets {
        println!(
            "{:<36}  {:<13}  {:<15}  {:<10}  {}",
            t.id,
            t.status.as_str(),
            t.kind.as_str(),
            fix.as_deref().unwrap_or("-"),
            t.title
        );
    }
    println!("({} tickets)", tickets.len());
}

fn print(value: &impl Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_list_filters() {
        let cli = Cli::try_parse_from([
            "ticket",
            "list",
            "--status",
            "forwarded",
            "--client",
            "acme",
            "--limit",
            "5",
        ])
        .unwrap();
        let Command::List {
            status,
            client,
            limit,
            json,
        } = cli.command
        else {
            panic!("expected list");
        };
        assert_eq!(status, Some(TicketStatus::Forwarded));
        assert_eq!(client.unwrap().as_str(), "acme");
        assert_eq!(limit, Some(5));
        assert!(!json);
        assert!(Cli::try_parse_from(["ticket", "list", "--status", "bogus"]).is_err());
    }

    #[test]
    fn parses_submit_with_typed_arguments() {
        let cli = Cli::try_parse_from([
            "ticket",
            "submit",
            "--client",
            "acme",
            "--title",
            "Login fails",
            "--severity",
            "high",
            "--label",
            "sso",
            "--label",
            "eu",
            "--kind",
            "bug",
            "--component",
            "auth",
            "--repo",
            "web-app",
            "--link",
            "https://logs.acme.test/1",
        ])
        .unwrap();
        let Command::Submit {
            severity,
            labels,
            kind,
            components,
            repo,
            links,
            ..
        } = cli.command
        else {
            panic!("expected submit");
        };
        assert_eq!(severity, Some(Severity::High));
        assert_eq!(labels, vec!["sso", "eu"]);
        assert_eq!(kind, TicketKind::Bug);
        assert_eq!(components, vec!["auth"]);
        assert_eq!(repo.unwrap().as_str(), "web-app");
        assert_eq!(links, vec!["https://logs.acme.test/1"]);
        assert!(
            Cli::try_parse_from([
                "ticket", "submit", "--client", "acme", "--title", "x", "--repo", "../etc"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from(["ticket", "submit", "--client", "bad id", "--title", "x"])
                .is_err()
        );
    }
}
