//! HTTP routes.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use ticket_api::{
    HealthResponse, ImportHistoryRequest, ImportHistoryResponse, KnowledgeEntry, ListTicketsQuery,
    ListedTicket, RecordResolutionRequest, SettingsRequest, SettingsResponse, SubmitTicketRequest,
    SubmitTicketResponse, TicketId, TicketListResponse, TicketView,
};
use ticket_core::{Error, Resolution};
use ticket_jira::{JiraEvent, SIGNATURE_HEADER, parse_webhook, verify_signature};
use tower_http::trace::TraceLayer;
use tracing::{debug, info};

use crate::error::ApiError;
use crate::state::AppState;

/// Maximum accepted request body size.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Builds the application router.
///
/// ```text
/// GET  /                                demo UI (static, no data)
/// GET  /healthz
/// POST /v1/tickets                       bearer token
/// GET  /v1/tickets?status=&client=&limit= bearer token
/// GET  /v1/tickets/{id}                  bearer token
/// POST /v1/tickets/{id}/triage           bearer token
/// POST /v1/tickets/{id}/fix              bearer token
/// POST /v1/tickets/{id}/resolution       bearer token
/// POST /v1/knowledge/import              bearer token
/// GET  /v1/settings                      bearer token
/// PUT  /v1/settings                      bearer token, if allow_mode_switch
/// POST /v1/webhooks/jira/{connector}     HMAC signature
/// ```
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/tickets", post(submit_ticket).get(list_tickets))
        .route("/tickets/{id}", get(get_ticket))
        .route("/tickets/{id}/triage", post(triage_ticket))
        .route("/tickets/{id}/fix", post(fix_ticket))
        .route("/tickets/{id}/resolution", post(record_resolution))
        .route("/knowledge/import", post(import_history))
        .route("/settings", get(get_settings).merge(put(put_settings)))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    // Webhooks authenticate with their own signatures, not the API token.
    let webhooks = Router::new().route("/webhooks/jira/{connector}", post(jira_webhook));

    Router::new()
        .route("/healthz", get(health))
        .merge(crate::ui::routes())
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
        auto_triage: state.auto_triage(),
        mode_switchable: state.mode_switchable,
        fixes_enabled: state.pipeline.fixes_enabled(),
        repositories: state
            .pipeline
            .fix_coordinator()
            .map(|c| c.directory().ids().map(ToString::to_string).collect())
            .unwrap_or_default(),
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

async fn list_tickets(
    State(state): State<AppState>,
    Query(query): Query<ListTicketsQuery>,
) -> Result<Json<TicketListResponse>, ApiError> {
    let mut tickets = Vec::new();
    for ticket in state.pipeline.list_tickets(&query.to_filter()).await? {
        let fix = state
            .pipeline
            .latest_fix(ticket.id)
            .await?
            .map(|attempt| attempt.result.kind().to_owned());
        let triage = if query.with_triage.unwrap_or(false) {
            state
                .pipeline
                .ticket_with_triage(ticket.id)
                .await?
                .and_then(|(_, triage)| triage)
        } else {
            None
        };
        tickets.push(ListedTicket {
            ticket,
            fix,
            triage,
        });
    }
    Ok(Json(TicketListResponse { tickets }))
}

async fn get_settings(State(state): State<AppState>) -> Json<SettingsResponse> {
    Json(SettingsResponse {
        auto_triage: state.auto_triage(),
        mode_switchable: state.mode_switchable,
    })
}

/// Switches the triage mode. Turning automatic mode on also queues every
/// ticket still waiting in `received`, as a restart would.
async fn put_settings(
    State(state): State<AppState>,
    Json(request): Json<SettingsRequest>,
) -> Result<Json<SettingsResponse>, ApiError> {
    if !state.mode_switchable {
        return Err(Error::Conflict(
            "the triage mode cannot be changed at runtime (server.allow_mode_switch = false)"
                .into(),
        )
        .into());
    }
    let was = state.auto_triage();
    state.set_auto_triage(request.auto_triage);
    if request.auto_triage && !was {
        let pending = state.pipeline.recover(10_000).await?;
        info!(
            count = pending.len(),
            "automatic mode on; queueing waiting tickets"
        );
        for id in pending {
            state.enqueue(id);
        }
    } else if !request.auto_triage && was {
        info!("step-by-step mode on; new tickets wait for explicit triage");
    }
    Ok(Json(SettingsResponse {
        auto_triage: request.auto_triage,
        mode_switchable: true,
    }))
}

async fn get_ticket(
    State(state): State<AppState>,
    Path(id): Path<TicketId>,
) -> Result<Json<TicketView>, ApiError> {
    Ok(Json(ticket_view(&state, id).await?))
}

/// Runs triage now. Idempotent: an already triaged ticket returns as it is.
async fn triage_ticket(
    State(state): State<AppState>,
    Path(id): Path<TicketId>,
) -> Result<Json<TicketView>, ApiError> {
    state.pipeline.triage_only(id).await?;
    Ok(Json(ticket_view(&state, id).await?))
}

/// Attempts a code fix now. `409` explains why a ticket is not eligible.
async fn fix_ticket(
    State(state): State<AppState>,
    Path(id): Path<TicketId>,
) -> Result<Json<TicketView>, ApiError> {
    state.pipeline.fix_ticket(id).await?;
    Ok(Json(ticket_view(&state, id).await?))
}

async fn ticket_view(state: &AppState, id: TicketId) -> Result<TicketView, ApiError> {
    let (ticket, triage) = state
        .pipeline
        .ticket_with_triage(id)
        .await?
        .ok_or_else(|| Error::NotFound(format!("ticket {id}")))?;
    let fix = state.pipeline.latest_fix(id).await?;
    let fix_skipped = match (&triage, &fix) {
        (Some(report), None) => state.pipeline.fix_skip_reason(&ticket, report),
        _ => None,
    };
    Ok(TicketView {
        ticket,
        triage,
        fix,
        fix_skipped,
    })
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
        patch: request.patch,
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
            let ingested = state.pipeline.ingest(*new).await?;
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
