//! HTTP-level tests: a real server on an ephemeral port, driven through the
//! `ticket-client` SDK, the same way a standalone client would use it.

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use ticket_client::api::{
    HistoricalTicket, ImportHistoryRequest, ListTicketsQuery, RecordResolutionRequest,
    SettingsRequest, Severity, SubmitTicketRequest, TicketId, TicketStatus, TicketView,
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
                    patch: None,
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
                patch: None,
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
        .pipeline
        .fix_coordinator()
        .unwrap()
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

#[tokio::test]
async fn tickets_can_be_listed_and_filtered() {
    let client = Client::new(spawn_server("").await).unwrap();
    let first = client
        .submit_ticket(&submit("Security breach on admin account", ""))
        .await
        .unwrap();
    let mut other = submit("Invoice question", "");
    other.client = ClientId::new("globex").unwrap();
    let second = client.submit_ticket(&other).await.unwrap();
    wait_for_triage(&client, first.id).await;
    wait_for_triage(&client, second.id).await;

    let all = client
        .list_tickets(&ListTicketsQuery::default())
        .await
        .unwrap();
    assert_eq!(
        all.tickets.iter().map(|t| t.ticket.id).collect::<Vec<_>>(),
        vec![second.id, first.id],
        "newest first"
    );

    let acme = ListTicketsQuery {
        client: Some(ClientId::new("acme").unwrap()),
        ..ListTicketsQuery::default()
    };
    let listed = client.list_tickets(&acme).await.unwrap();
    assert_eq!(listed.tickets.len(), 1);
    assert_eq!(listed.tickets[0].ticket.id, first.id);
    assert_eq!(listed.tickets[0].fix, None);

    let escalated = ListTicketsQuery {
        status: Some(TicketStatus::Escalated),
        limit: Some(1),
        ..ListTicketsQuery::default()
    };
    let listed = client.list_tickets(&escalated).await.unwrap();
    assert_eq!(listed.tickets.len(), 1);
    assert!(
        listed
            .tickets
            .iter()
            .all(|t| t.ticket.status == TicketStatus::Escalated)
    );
}

#[tokio::test]
async fn step_by_step_mode_waits_for_explicit_triage_and_fix() {
    let client = Client::new(spawn_server("[server]\nauto_triage = false\n").await).unwrap();
    let health = client.health().await.unwrap();
    assert!(!health.auto_triage);
    assert!(!health.fixes_enabled);

    let receipt = client
        .submit_ticket(&submit("Security breach on admin account", ""))
        .await
        .unwrap();
    // Nothing happens on its own.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let view = client.get_ticket(receipt.id).await.unwrap();
    assert_eq!(view.ticket.status, TicketStatus::Received);
    assert!(view.triage.is_none());

    // Fixing before triage is refused with a reason.
    let early = client.fix_ticket(receipt.id).await.unwrap_err();
    assert!(
        matches!(early, ClientError::Api { status: 409, ref message, .. } if message.contains("triage")),
        "{early:?}"
    );

    let triaged = client.triage_ticket(receipt.id).await.unwrap();
    assert_eq!(triaged.ticket.status, TicketStatus::Escalated);
    assert!(triaged.triage.is_some());
    assert!(triaged.fix.is_none(), "triage alone never fixes");
    // Triage is idempotent.
    let again = client.triage_ticket(receipt.id).await.unwrap();
    assert_eq!(again.triage, triaged.triage);

    // Fixes are disabled in this configuration, and the refusal says so.
    let refused = client.fix_ticket(receipt.id).await.unwrap_err();
    assert!(
        matches!(refused, ClientError::Api { status: 409, ref message, .. } if message.contains("disabled")),
        "{refused:?}"
    );
}

#[tokio::test]
async fn demo_ui_is_served_with_a_strict_content_security_policy() {
    let base = spawn_server("").await;
    let http = reqwest::Client::new();

    let page = http.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    let headers = page.headers().clone();
    assert!(
        headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'self'"));
    let html = page.text().await.unwrap();
    assert!(html.contains("/ui/app.js") && html.contains("/ui/style.css"));

    for (path, kind) in [
        ("/ui/app.js", "text/javascript"),
        ("/ui/style.css", "text/css"),
    ] {
        let asset = http.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(asset.status(), 200, "{path}");
        assert!(
            asset.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with(kind),
            "{path}"
        );
    }
}

#[tokio::test]
async fn mode_switching_is_refused_unless_allowed() {
    let client = Client::new(spawn_server("").await).unwrap();
    let settings = client.settings().await.unwrap();
    assert!(settings.auto_triage && !settings.mode_switchable);
    let err = client
        .update_settings(&SettingsRequest { auto_triage: false })
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Api { status: 409, ref message, .. } if message.contains("allow_mode_switch")),
        "{err:?}"
    );
    assert!(client.settings().await.unwrap().auto_triage);
}

#[tokio::test]
async fn switching_to_automatic_triages_waiting_tickets_and_escalations_feed_the_inbox() {
    let client = Client::new(
        spawn_server("[server]\nauto_triage = false\nallow_mode_switch = true\n").await,
    )
    .unwrap();
    assert!(client.health().await.unwrap().mode_switchable);

    // Step-by-step: the ticket waits.
    let waiting = client
        .submit_ticket(&submit(
            "Security breach on admin account",
            "Unauthorized access to the admin panel overnight.",
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        client.get_ticket(waiting.id).await.unwrap().ticket.status,
        TicketStatus::Received
    );

    // Automatic: the waiting ticket is picked up, and new ones flow through.
    let settings = client
        .update_settings(&SettingsRequest { auto_triage: true })
        .await
        .unwrap();
    assert!(settings.auto_triage);
    let view = wait_for_triage(&client, waiting.id).await;
    assert_eq!(view.ticket.status, TicketStatus::Escalated);
    let fresh = client
        .submit_ticket(&submit(
            "Production down for all users",
            "Full outage since the deploy.",
        ))
        .await
        .unwrap();
    wait_for_triage(&client, fresh.id).await;

    // The escalation inbox feed: escalated tickets with their triage and draft.
    let inbox = client
        .list_tickets(&ListTicketsQuery {
            status: Some(TicketStatus::Escalated),
            with_triage: Some(true),
            ..ListTicketsQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(inbox.tickets.len(), 2);
    for listed in &inbox.tickets {
        let triage = listed
            .triage
            .as_ref()
            .expect("with_triage includes the report");
        let draft = triage.draft.as_ref().expect("escalations carry a draft");
        assert!(!draft.first_steps.is_empty());
    }
    // Without with_triage the list stays light.
    let light = client
        .list_tickets(&ListTicketsQuery::default())
        .await
        .unwrap();
    assert!(light.tickets.iter().all(|t| t.triage.is_none()));

    // The ticket view says why no code fix was attempted.
    let view = client.get_ticket(fresh.id).await.unwrap();
    assert!(view.fix.is_none());
    assert!(view.fix_skipped.is_some(), "{view:?}");

    // Back to step-by-step: new tickets wait again.
    client
        .update_settings(&SettingsRequest { auto_triage: false })
        .await
        .unwrap();
    let later = client
        .submit_ticket(&submit("Another breach report", "security breach"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        client.get_ticket(later.id).await.unwrap().ticket.status,
        TicketStatus::Received
    );
}

#[tokio::test]
async fn escalation_inbox_page_is_served() {
    let base = spawn_server("").await;
    let http = reqwest::Client::new();
    let page = http.get(format!("{base}/inbox")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.headers().contains_key("content-security-policy"));
    let html = page.text().await.unwrap();
    assert!(html.contains("/ui/inbox.js") && html.contains("/ui/common.js"));
    for path in ["/ui/inbox.js", "/ui/common.js"] {
        let asset = http.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(asset.status(), 200, "{path}");
    }
}
