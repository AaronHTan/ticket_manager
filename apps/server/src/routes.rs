//! HTTP routes.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use ticket_api::{
    HealthResponse, ImportHistoryRequest, ImportHistoryResponse, KnowledgeEntry,
    RecordResolutionRequest, SubmitTicketRequest, SubmitTicketResponse, TicketId, TicketView,
};
use ticket_core::{Error, Resolution};
use ticket_jira::{JiraEvent, SIGNATURE_HEADER, parse_webhook, verify_signature};
use tower_http::trace::TraceLayer;
use tracing::debug;

use crate::error::ApiError;
use crate::state::AppState;

/// Maximum accepted request body size.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Builds the application router.
///
/// ```text
/// GET  /healthz
/// POST /v1/tickets                       bearer token
/// GET  /v1/tickets/{id}                  bearer token
/// POST /v1/tickets/{id}/resolution       bearer token
/// POST /v1/knowledge/import              bearer token
/// POST /v1/webhooks/jira/{connector}     HMAC signature
/// ```
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/tickets", post(submit_ticket))
        .route("/tickets/{id}", get(get_ticket))
        .route("/tickets/{id}/resolution", post(record_resolution))
        .route("/knowledge/import", post(import_history))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    // Webhooks authenticate with their own signatures, not the API token.
    let webhooks = Router::new().route("/webhooks/jira/{connector}", post(jira_webhook));

    Router::new()
        .route("/healthz", get(health))
        .nest("/v1", api.merge(webhooks))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    Ok(Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        knowledge_entries: state.pipeline.knowledge_size().await?,
    }))
}

async fn submit_ticket(
    State(state): State<AppState>,
    Json(request): Json<SubmitTicketRequest>,
) -> Result<(StatusCode, Json<SubmitTicketResponse>), ApiError> {
    let ingested = state.pipeline.ingest(request.into()).await?;
    if ingested.created {
        state.enqueue(ingested.ticket.id);
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitTicketResponse {
            id: ingested.ticket.id,
            status: ingested.ticket.status,
            duplicate: !ingested.created,
        }),
    ))
}

async fn get_ticket(
    State(state): State<AppState>,
    Path(id): Path<TicketId>,
) -> Result<Json<TicketView>, ApiError> {
    let (ticket, triage) = state
        .pipeline
        .ticket_with_triage(id)
        .await?
        .ok_or_else(|| Error::NotFound(format!("ticket {id}")))?;
    Ok(Json(TicketView { ticket, triage }))
}

async fn record_resolution(
    State(state): State<AppState>,
    Path(id): Path<TicketId>,
    Json(request): Json<RecordResolutionRequest>,
) -> Result<Json<KnowledgeEntry>, ApiError> {
    let resolution = Resolution {
        summary: request.summary,
        automatable: request.automatable,
        resolved_by: request.resolved_by,
    };
    let entry = state
        .pipeline
        .record_resolution(id, resolution, request.category, request.severity)
        .await?;
    Ok(Json(entry))
}

async fn import_history(
    State(state): State<AppState>,
    Json(request): Json<ImportHistoryRequest>,
) -> Result<Json<ImportHistoryResponse>, ApiError> {
    let imported = state.pipeline.import_history(request.tickets).await?;
    Ok(Json(ImportHistoryResponse { imported }))
}

async fn jira_webhook(
    State(state): State<AppState>,
    Path(connector): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let config = state
        .jira
        .get(&connector)
        .ok_or_else(|| Error::NotFound(format!("jira connector `{connector}`")))?;

    if let Some(secret) = &config.webhook_secret {
        let signature = headers
            .get(SIGNATURE_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if !verify_signature(secret, &body, signature) {
            return Err(ApiError::Unauthorized);
        }
    }

    match parse_webhook(&connector, config, &body)? {
        JiraEvent::Created(new) => {
            let ingested = state.pipeline.ingest(new).await?;
            if ingested.created {
                state.enqueue(ingested.ticket.id);
            }
        }
        JiraEvent::Resolved {
            external_ref,
            summary,
            resolved_by,
        } => {
            state
                .pipeline
                .record_external_resolution(&external_ref, summary, resolved_by)
                .await?;
        }
        JiraEvent::Ignored(reason) => debug!(%connector, %reason, "ignored jira webhook"),
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if let Some(expected) = &state.api_token {
        let provided = request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if !provided.is_some_and(|p| constant_time_eq(p.as_bytes(), expected.as_bytes())) {
            return Err(ApiError::Unauthorized);
        }
    }
    Ok(next.run(request).await)
}

/// Compares two byte strings in time that depends only on their lengths.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_eq_matches_equality() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"tokem"));
        assert!(!constant_time_eq(b"token", b"token2"));
    }
}
