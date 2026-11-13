//! The demo web UI: three static files compiled into the binary.
//!
//! The page is plain HTML, CSS and JavaScript with no build step. It talks
//! to the same `/v1` API as the CLI, so it needs no server-side state of its
//! own. The files carry no data, so they are served without the API token;
//! the page asks for the token if the API requires one.

use axum::Router;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue};
use axum::response::IntoResponse;
use axum::routing::get;

use crate::state::AppState;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const STYLE_CSS: &str = include_str!("../ui/style.css");

/// Everything is same-origin, so the policy can forbid all else.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
    connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; \
    frame-ancestors 'none'";

/// `GET /`, `/ui/app.js` and `/ui/style.css`.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(|| async { page(INDEX_HTML) }))
        .route(
            "/ui/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .route(
            "/ui/style.css",
            get(|| async { asset("text/css; charset=utf-8", STYLE_CSS) }),
        )
}

fn page(body: &'static str) -> impl IntoResponse {
    (
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (
                HeaderName::from_static("content-security-policy"),
                HeaderValue::from_static(CSP),
            ),
            (
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            ),
        ],
        body,
    )
}

fn asset(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            ),
        ],
        body,
    )
}
