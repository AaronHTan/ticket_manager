//! `ticket-seed`: fills a database with fixture history and tickets.
//!
//! ```bash
//! cargo run -p ticket-server --bin ticket-seed -- --config config/test.toml --reset
//! ```
//!
//! Exits non-zero if any fixture ticket does not reach its expected status.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::Parser;
use ticket_server::AppConfig;
use ticket_server::bootstrap::{build_pipeline, sqlite_path};
use ticket_server::seed::{load_history, load_tickets, seed};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(about = "Seed a ticket-handler database with fixture data")]
struct Args {
    /// Configuration file. Its `database.url` is the database seeded.
    #[arg(
        long,
        env = "TICKET_HANDLER_CONFIG",
        default_value = "config/test.toml"
    )]
    config: PathBuf,
    /// Resolved tickets to index into the knowledge base.
    #[arg(long, default_value = "fixtures/history.json")]
    history: PathBuf,
    /// Incoming tickets to ingest and triage.
    #[arg(long, default_value = "fixtures/tickets.json")]
    tickets: PathBuf,
    /// Delete the SQLite database first, so seeding starts from nothing.
    #[arg(long)]
    reset: bool,
    /// Allow --reset on a database whose file name does not contain "test".
    #[arg(long, requires = "reset")]
    force: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            // Fixture tickets reference no real tracker, so the per-ticket
            // "no tracker registered" warning is expected noise here.
            EnvFilter::new("warn,ticket_engine::dispatch=error")
        }))
        .init();
    let args = Args::parse();
    let config = AppConfig::load(&args.config)
        .with_context(|| format!("loading configuration from {}", args.config.display()))?;
    let url = &config.database.url;

    // Read fixtures before touching the database, so a typo cannot leave it empty.
    let history = load_history(&args.history)?;
    let tickets = load_tickets(&args.tickets)?;

    if args.reset {
        reset(url, args.force)?;
    }
    let pipeline = build_pipeline(&config).await?;
    let report = seed(&pipeline, history, tickets).await?;

    println!("database: {url}");
    match report.imported {
        Some(n) => println!("knowledge: imported {n} entries"),
        None => println!(
            "knowledge: already had entries, history not re-imported (use --reset to start over)"
        ),
    }
    println!("knowledge entries: {}\n", report.knowledge_entries);
    println!(
        "{:<7} {:<8} {:<14} {:<14} {:<6} title",
        "key", "", "expected", "actual", ""
    );
    for t in &report.tickets {
        println!(
            "{:<7} {:<8} {:<14} {:<14} {:<6} {}",
            t.key,
            if t.created { "new" } else { "existing" },
            t.expected.as_str(),
            t.actual.as_str(),
            if t.matches() { "ok" } else { "FAIL" },
            t.title
        );
    }

    let failures = report.mismatches().count();
    if failures > 0 {
        eprintln!("\n{failures} ticket(s) did not reach their expected status");
        return Ok(ExitCode::FAILURE);
    }
    println!(
        "\nall {} tickets reached their expected status",
        report.tickets.len()
    );
    Ok(ExitCode::SUCCESS)
}

/// Deletes a SQLite database and its WAL files.
///
/// Refuses anything that is not a SQLite file, and, without `force`, any
/// file whose name does not mark it as a test database.
fn reset(url: &str, force: bool) -> anyhow::Result<()> {
    let Some(path) = sqlite_path(url) else {
        bail!("--reset only works on SQLite file databases, not `{url}`");
    };
    let is_test = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("test"));
    if !is_test && !force {
        bail!(
            "refusing to reset `{}`: its name does not contain \"test\" (pass --force to override)",
            path.display()
        );
    }
    for file in [
        path.to_path_buf(),
        with_suffix(path, "-wal"),
        with_suffix(path, "-shm"),
    ] {
        match std::fs::remove_file(&file) {
            Ok(()) => println!("removed {}", file.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", file.display())),
        }
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}
