//! `ticket-server`: the long-running ticket triage service.
//!
//! Configuration is read from `$TICKET_HANDLER_CONFIG` (default
//! `config/default.toml`), then overridden by `TICKET_HANDLER_*` environment
//! variables.

use std::path::PathBuf;

use anyhow::Context;
use ticket_server::config::LogFormat;
use ticket_server::{App, AppConfig, build, router};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = std::env::var_os("TICKET_HANDLER_CONFIG")
        .map_or_else(|| PathBuf::from("config/default.toml"), PathBuf::from);
    let config = AppConfig::load(&config_path)
        .with_context(|| format!("loading configuration from {}", config_path.display()))?;
    init_tracing(config.server.log_format);

    let App { state, workers } = build(&config).await?;
    let listener = TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("binding {}", config.server.bind))?;
    info!(addr = %listener.local_addr()?, "ticket-server listening");

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // The router (and its queue handle) is dropped now, so workers drain and exit.
    info!("http server stopped; draining triage queue");
    workers.join().await;
    Ok(())
}

fn init_tracing(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,ticket_server=debug,ticket_engine=debug,tower_http=info")
    });
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match format {
        LogFormat::Pretty => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    info!("shutdown signal received");
}
