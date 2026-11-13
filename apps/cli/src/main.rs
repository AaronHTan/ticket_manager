//! `ticket`: a standalone command-line client for the ticket-handler service.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde::Serialize;
use ticket_client::Client;
use ticket_client::api::{
    Category, ClientId, CodeContext, HistoricalTicket, ImportHistoryRequest, Link,
    RecordResolutionRequest, RepoId, Reporter, Severity, SubmitTicketRequest, TicketId, TicketKind,
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
        Command::Get { id } => print(&client.get_ticket(id).await?),
        Command::Resolve {
            id,
            summary,
            automatable,
            resolved_by,
            category,
            severity,
        } => {
            let request = RecordResolutionRequest {
                summary,
                automatable,
                resolved_by,
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
