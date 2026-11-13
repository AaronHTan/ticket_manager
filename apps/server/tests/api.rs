//! HTTP-level tests: a real server on an ephemeral port, driven through the
//! `ticket-client` SDK, the same way a standalone client would use it.

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use ticket_client::api::{
    HistoricalTicket, ImportHistoryRequest, RecordResolutionRequest, Severity, SubmitTicketRequest,
    TicketId, TicketStatus, TicketView,
};
use ticket_client::{Client, ClientError};
use ticket_core::{
    Category, ClientId, CodeContext, Link, RepoId, Reporter, Resolution, RoutingDecision,
    TicketKind,
};
use ticket_server::{AppConfig, build, router};
use tokio::net::TcpListener;

const BASE_CONFIG: &str = r#"
[database]
url = "memory"

[escalation.intermediate]
name = "L2 Support"

[escalation.default_expert]
name = "Platform On-Call"

[escalation.experts.security]
name = "Security Team"
"#;

/// Starts a server and returns its base URL.
async fn spawn_server(extra_config: &str) -> String {
    let config = AppConfig::from_toml(&format!("{BASE_CONFIG}\n{extra_config}")).unwrap();
    let app = build(&config).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(app.state)).await.unwrap();
    });
    format!("http://{addr}")
}

/// Polls until triage has finished for `id`.
async fn wait_for_triage(client: &Client, id: TicketId) -> TicketView {
    for _ in 0..100 {
        let view = client.get_ticket(id).await.unwrap();
        if !matches!(
            view.ticket.status,
            TicketStatus::Received | TicketStatus::Triaging
        ) {
            return view;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("ticket {id} was not triaged in time");
}

fn submit(title: &str, description: &str) -> SubmitTicketRequest {
    let mut request = SubmitTicketRequest::new(ClientId::new("acme").unwrap(), title, description);
    request.reporter = Some(Reporter::from_label("jane@acme.test").unwrap());
    request
}

#[tokio::test]
async fn standalone_client_flow_import_submit_triage_learn() {
    let client = Client::new(spawn_server("").await).unwrap();
    assert_eq!(client.health().await.unwrap().knowledge_entries, 0);

    let imported = client
        .import_history(&ImportHistoryRequest {
            tickets: vec![HistoricalTicket {
                title: "Password reset email not received".into(),
                description: "The reset email never arrived.".into(),
                category: Some(Category::new("authentication").unwrap()),
                severity: Severity::Low,
                resolution: Resolution {
                    summary: "Re-send the reset link from the admin console.".into(),
                    automatable: true,
                    resolved_by: None,
                },
            }],
        })
        .await
        .unwrap();
    assert_eq!(imported.imported, 1);

    // A known issue is auto-resolved.
    let receipt = client
        .submit_ticket(&submit(
            "Password reset email not received",
            "The reset email never arrived.",
        ))
        .await
        .unwrap();
    assert_eq!(receipt.status, TicketStatus::Received);
    let view = wait_for_triage(&client, receipt.id).await;
    assert_eq!(view.ticket.status, TicketStatus::AutoResolved);
    assert!(matches!(
        view.triage.unwrap().decision,
        RoutingDecision::AutoResolve { .. }
    ));

    // A novel security issue is escalated to the security expert.
    let receipt = client
        .submit_ticket(&submit(
            "Possible security breach",
            "Unauthorized access to the admin panel overnight.",
        ))
        .await
        .unwrap();
    let view = wait_for_triage(&client, receipt.id).await;
    assert_eq!(view.ticket.status, TicketStatus::Escalated);
    let triage = view.triage.unwrap();
    assert_eq!(triage.classification.severity.severity, Severity::Critical);
    assert!(
        matches!(triage.decision, RoutingDecision::Escalate { ref assignee, .. } if assignee.name == "Security Team")
    );

    // The expert's fix is fed back into the knowledge base.
    let entry = client
        .record_resolution(
            receipt.id,
            &RecordResolutionRequest {
                summary: "Rotated admin credentials and enforced MFA.".into(),
                automatable: false,
                resolved_by: Some("security".into()),
                category: None,
                severity: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(entry.category.unwrap().as_str(), "security");
    assert_eq!(client.health().await.unwrap().knowledge_entries, 2);
    assert_eq!(
        client.get_ticket(receipt.id).await.unwrap().ticket.status,
        TicketStatus::Resolved
    );
}

#[tokio::test]
async fn errors_map_to_status_codes() {
    let client = Client::new(spawn_server("").await).unwrap();

    let missing = client.get_ticket(TicketId::new()).await.unwrap_err();
    assert!(
        matches!(missing, ClientError::Api { status: 404, ref code, .. } if code == "not_found")
    );

    let invalid = client.submit_ticket(&submit("   ", "")).await.unwrap_err();
    assert!(matches!(invalid, ClientError::Api { status: 422, .. }));
}

#[tokio::test]
async fn api_token_is_enforced_except_on_health() {
    let base = spawn_server("[server]\napi_token = \"s3cret\"").await;

    let anonymous = Client::new(&base).unwrap();
    assert!(anonymous.health().await.is_ok());
    let err = anonymous
        .submit_ticket(&submit("Hi", ""))
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Api { status: 401, .. }));

    let wrong = Client::new(&base).unwrap().with_token("nope");
    assert!(matches!(
        wrong.submit_ticket(&submit("Hi", "")).await.unwrap_err(),
        ClientError::Api { status: 401, .. }
    ));

    let authorised = Client::new(&base).unwrap().with_token("s3cret");
    assert!(authorised.submit_ticket(&submit("Hi", "")).await.is_ok());
}

#[tokio::test]
async fn jira_webhook_requires_valid_signature_and_is_idempotent() {
    // The Jira base URL points at a closed port. Outbound calls fail, which
    // this test tolerates because it only covers inbound handling.
    let base = spawn_server(
        r#"
[connectors.jira.acme]
client = "acme"
base_url = "http://127.0.0.1:9"
email = "bot@acme.test"
api_token = "token"
webhook_secret = "hook-secret"
"#,
    )
    .await;
    let http = reqwest::Client::new();
    let url = format!("{base}/v1/webhooks/jira/acme");
    let body = serde_json::json!({
        "webhookEvent": "jira:issue_created",
        "issue": { "key": "OPS-7", "fields": { "summary": "Printer on fire", "description": "Floor 3" } }
    })
    .to_string();
    let sign = |payload: &str| {
        let mut mac = Hmac::<Sha256>::new_from_slice(b"hook-secret").unwrap();
        mac.update(payload.as_bytes());
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    };

    let unsigned = http.post(&url).body(body.clone()).send().await.unwrap();
    assert_eq!(unsigned.status(), 401);

    let forged = http
        .post(&url)
        .header("x-hub-signature", sign("different body"))
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), 401);

    for _ in 0..2 {
        let signed = http
            .post(&url)
            .header("x-hub-signature", sign(&body))
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(signed.status(), 204);
    }

    let unknown = http
        .post(format!("{base}/v1/webhooks/jira/nope"))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
}

#[tokio::test]
async fn ticket_metadata_round_trips_over_http() {
    let client = Client::new(spawn_server("").await).unwrap();
    let mut request = submit("Checkout 500 on empty cart", "TypeError in cart.total");
    request.reporter = Some(Reporter {
        name: Some("Jane Doe".into()),
        email: Some("jane@acme.test".into()),
        external_id: None,
    });
    request.kind = TicketKind::Bug;
    request.components = vec!["checkout".into()];
    request.code_context = Some(CodeContext {
        repository: Some(RepoId::new("shop").unwrap()),
        git_ref: None,
        paths: vec!["cart.py".into()],
        version: Some("3.1.0".into()),
    });
    request.links = vec![Link::new("https://logs.acme.test/run/7")];
    request.metadata.insert("browser".into(), "firefox".into());

    let receipt = client.submit_ticket(&request).await.unwrap();
    let ticket = client.get_ticket(receipt.id).await.unwrap().ticket;
    assert_eq!(ticket.reporter, request.reporter);
    assert_eq!(ticket.kind, TicketKind::Bug);
    assert_eq!(ticket.components, request.components);
    assert_eq!(ticket.code_context, request.code_context);
    assert_eq!(ticket.links, request.links);
    assert_eq!(ticket.metadata, request.metadata);

    // Paths that escape the repository are rejected at the edge.
    let mut bad = submit("x", "");
    bad.code_context = Some(CodeContext {
        paths: vec!["../../etc/passwd".into()],
        ..CodeContext::default()
    });
    let err = client.submit_ticket(&bad).await.unwrap_err();
    assert!(matches!(err, ClientError::Api { status: 422, .. }));
}

#[tokio::test]
async fn configured_repositories_require_a_working_git() {
    let repos = r#"
[repositories.shop]
remote = "git@git.test:acme/shop.git"
clients = ["acme"]
components = ["checkout"]
"#;
    let broken = AppConfig::from_toml(&format!(
        "{BASE_CONFIG}\n[git]\nbinary = \"/nonexistent/git\"\n{repos}"
    ))
    .unwrap();
    let err = build(&broken).await.err().expect("startup must fail");
    assert!(format!("{err:#}").contains("could not be run"), "{err:#}");

    let working = AppConfig::from_toml(&format!("{BASE_CONFIG}\n{repos}")).unwrap();
    let app = build(&working).await.unwrap();
    let ids: Vec<_> = app
        .state
        .fixes
        .directory()
        .ids()
        .map(|id| id.to_string())
        .collect();
    assert_eq!(ids, ["shop"]);

    let invalid = AppConfig::from_toml(&format!(
        "{BASE_CONFIG}\n[repositories.shop]\nremote = \"--upload-pack=evil\"\n"
    ))
    .unwrap();
    assert!(build(&invalid).await.is_err());
}
