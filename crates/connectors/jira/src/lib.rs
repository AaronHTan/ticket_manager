//! Jira Cloud connector.
//!
//! * **Inbound**: [`parse_webhook`] turns Jira webhook payloads into domain
//!   events, and [`verify_signature`] authenticates them (HMAC-SHA256).
//! * **Outbound**: [`JiraClient`] implements
//!   [`Tracker`](ticket_core::ports::Tracker) on the Jira REST API v3.
//!
//! Other connectors (ServiceNow, Zendesk, GitHub Issues) follow the same
//! shape: a config struct, a webhook parser producing `NewTicket`s, and a
//! `Tracker` implementation. Everything else is shared.

mod adf;
mod client;
mod config;
mod priority;
mod webhook;

pub use adf::{adf_to_text, text_to_adf};
pub use client::JiraClient;
pub use config::JiraConfig;
pub use priority::{priority_for_severity, severity_from_priority};
pub use webhook::{JiraEvent, SIGNATURE_HEADER, parse_webhook, verify_signature};
