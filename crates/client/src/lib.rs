//! Async SDK for the ticket-handler HTTP API.
//!
//! This is the shared building block for standalone clients: the bundled
//! CLI, internal tools, or a customer's own integration that does not use
//! Jira.
//!
//! ```no_run
//! # async fn demo() -> Result<(), ticket_client::ClientError> {
//! use ticket_client::{Client, api::SubmitTicketRequest};
//!
//! let client = Client::new("http://127.0.0.1:8080")?.with_token("secret");
//! let request =
//!     SubmitTicketRequest::new("acme".parse().unwrap(), "Cannot log in", "SSO redirect loops");
//! let receipt = client.submit_ticket(&request).await?;
//! let view = client.get_ticket(receipt.id).await?;
//! println!("{:?}", view.ticket.status);
//! # Ok(()) }
//! ```

use std::time::Duration;

use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
pub use ticket_api as api;
use ticket_api::{
    ErrorResponse, HealthResponse, ImportHistoryRequest, ImportHistoryResponse, KnowledgeEntry,
    ListTicketsQuery, RecordResolutionRequest, SubmitTicketRequest, SubmitTicketResponse, TicketId,
    TicketListResponse, TicketView, paths,
};

/// Errors returned by [`Client`].
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Network, TLS or decoding failure.
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// The server answered with a non-success status.
    #[error("server returned {status} ({code}): {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Machine-readable error code.
        code: String,
        /// Human-readable message.
        message: String,
    },
    /// The base URL is unusable.
    #[error("invalid base url: {0}")]
    InvalidUrl(String),
}

/// A typed client for one ticket-handler server.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl Client {
    /// Creates a client for the server at `base_url`, such as `http://127.0.0.1:8080`.
    pub fn new(base_url: impl Into<String>) -> Result<Self, ClientError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(ClientError::InvalidUrl(base_url));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("ticket-client/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url,
            token: None,
        })
    }

    /// Sends `Authorization: Bearer <token>` with every request.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// `GET /healthz`
    pub async fn health(&self) -> Result<HealthResponse, ClientError> {
        self.execute(self.request(Method::GET, paths::HEALTH)).await
    }

    /// `POST /v1/tickets`
    pub async fn submit_ticket(
        &self,
        request: &SubmitTicketRequest,
    ) -> Result<SubmitTicketResponse, ClientError> {
        self.send_json(Method::POST, paths::TICKETS, request).await
    }

    /// `GET /v1/tickets`
    pub async fn list_tickets(
        &self,
        query: &ListTicketsQuery,
    ) -> Result<TicketListResponse, ClientError> {
        let path = format!("{}{}", paths::TICKETS, query.to_query_string());
        self.execute(self.request(Method::GET, &path)).await
    }

    /// `GET /v1/tickets/{id}`
    pub async fn get_ticket(&self, id: TicketId) -> Result<TicketView, ClientError> {
        self.execute(self.request(Method::GET, &paths::ticket(id)))
            .await
    }

    /// `POST /v1/tickets/{id}/resolution`
    pub async fn record_resolution(
        &self,
        id: TicketId,
        request: &RecordResolutionRequest,
    ) -> Result<KnowledgeEntry, ClientError> {
        self.send_json(Method::POST, &paths::ticket_resolution(id), request)
            .await
    }

    /// `POST /v1/knowledge/import`
    pub async fn import_history(
        &self,
        request: &ImportHistoryRequest,
    ) -> Result<ImportHistoryResponse, ClientError> {
        self.send_json(Method::POST, paths::KNOWLEDGE_IMPORT, request)
            .await
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let builder = self
            .http
            .request(method, format!("{}{path}", self.base_url));
        match &self.token {
            Some(token) => builder.bearer_auth(token),
            None => builder,
        }
    }

    async fn send_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<T, ClientError> {
        self.execute(self.request(method, path).json(body)).await
    }

    async fn execute<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, ClientError> {
        let response = request.send().await?;
        if response.status().is_success() {
            return Ok(response.json().await?);
        }
        Err(api_error(response).await)
    }
}

async fn api_error(response: Response) -> ClientError {
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    match serde_json::from_str::<ErrorResponse>(&text) {
        Ok(body) => ClientError::Api {
            status,
            code: body.code,
            message: body.message,
        },
        Err(_) => ClientError::Api {
            status,
            code: "unknown".into(),
            message: text,
        },
    }
}
