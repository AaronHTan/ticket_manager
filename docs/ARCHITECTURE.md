# Architecture

## Goals

1. **Always-on intake.** A long-running server accepts tickets from many
   external clients: Jira webhooks today, other trackers and direct API
   callers later.
2. **Triage by precedent.** Read the ticket, find similar resolved tickets
   (retrieval), assess severity, and decide whether to auto-resolve, forward
   to intermediate support, or escalate to an expert.
3. **Learn.** Every human resolution becomes new knowledge.
4. **Modular.** Each new client, tracker, store or model is a new adapter
   behind an existing trait. The engine does not change.

## Style: hexagonal (ports and adapters)

```mermaid
flowchart LR
    subgraph Inbound
        JW[Jira webhook] --> S
        API[REST API] --> S
        CLI[ticket CLI / SDK] --> API
    end
    subgraph S[apps/server - composition root]
        R[axum routes] --> P
    end
    subgraph Engine[ticket-engine]
        P[TriagePipeline] --> Q[(TriageQueue)] --> W[WorkerPool]
        W --> P
        P --> C[Classifier]
        P --> RT[TicketRouter - pure]
        P --> D[Dispatcher]
    end
    subgraph Ports[ticket-core ports]
        TR[TicketRepository]
        KB[KnowledgeBase]
        EM[Embedder]
        TK[Tracker]
        NT[Notifier]
    end
    P --> TR & KB & EM
    D --> TK & NT
    TR & KB -.impl.- SQL[(SQLite / in-memory)]
    EM -.impl.- HE[HashingEmbedder]
    TK -.impl.- JC[JiraClient]
    NT -.impl.- LN[LogNotifier]
```

* **`ticket-core`** holds the domain types (`Ticket`, `Severity`,
  `RoutingDecision`, ...) and the **port traits**. It has no I/O and no
  framework dependencies.
* **`ticket-engine`** holds the application logic, written only against those traits.
* **Adapters** (`ticket-storage`, `ticket-retrieval`, `ticket-jira`)
  implement the ports. Their heavy dependencies (sqlx, reqwest) stay out of
  the engine.
* **`apps/server`** is the only place that names concrete adapters
  (`bootstrap.rs`). Swapping SQLite for Postgres, or the hashing embedder
  for a neural model, is a change there plus a new adapter crate.

### Why `Arc<dyn Trait>` and not generics

Adapters are chosen **from configuration at runtime** (`database.url =
"memory"` vs SQLite, any number of Jira connectors), and a single process
serves several clients with different trackers. Trait objects make that
direct. The cost of one virtual call per I/O operation is negligible next
to the I/O itself. Pure, hot-path logic (`TicketRouter::decide`,
`RuleBasedClassifier::classify_ticket`, `Embedding::cosine`) is plain
synchronous code with no dynamic dispatch.

`async-trait` is still used for the ports because native `async fn` in
traits is not yet dyn-compatible on stable Rust.

## Ticket lifecycle

```mermaid
stateDiagram-v2
    [*] --> Received: ingest
    Received --> Triaging: worker claims (CAS)
    Failed --> Triaging: retry
    Triaging --> AutoResolved
    Triaging --> Forwarded
    Triaging --> Escalated
    Triaging --> Failed: dispatch error
    Forwarded --> Resolved: human fix recorded
    Escalated --> Resolved: human fix recorded
```

* **Ingest** validates input, deduplicates by `ExternalRef` (webhooks are
  delivered at least once), persists the ticket as `received` and enqueues
  its id. The HTTP response is `202 Accepted`. Triage is asynchronous.
* **Claim**: `TicketRepository::transition(id, [Received, Failed], Triaging)`
  is an atomic compare-and-set, so a ticket enqueued twice is processed once.
* **Recovery**: on start-up, tickets stuck in `triaging` are reset and every
  `received` ticket is re-queued. The in-process queue therefore never loses
  work, because the database is the source of truth.

## The triage pipeline

```
embed(title + description)
  -> KnowledgeBase::search(top_k)                    retrieval
  -> Classifier::classify(ticket, matches)            severity + category
  -> TicketRouter::decide(classification, matches)    pure decision
  -> save TriageReport (append-only audit)
  -> Dispatcher::dispatch                             Tracker + Notifier
  -> status = decision.resulting_status()
```

### Severity (`RuleBasedClassifier`)

Three signals, combined by **maximum** (under-triage costs more than over-triage):

1. Keyword rules on word boundaries (`"security breach"` gives critical/security).
2. The reporter's priority (a Jira `Highest` maps to critical), treated as a hint.
3. A similarity-weighted vote over the severities of matching past tickets.

Confidence rises with agreement between signals. Every contribution is
recorded in `rationale` so decisions are auditable.

### Routing (`TicketRouter`, pure)

| Condition (checked in order)                                                         | Decision                     |
|--------------------------------------------------------------------------------------|------------------------------|
| severity >= `escalate_severity` (default critical)                                   | Escalate to category expert  |
| best score >= `auto_resolve_threshold` (0.90) **and** fix automatable **and** severity <= `max_auto_resolve_severity` (medium) | Auto-resolve |
| best score >= `forward_threshold` (0.65)                                             | Forward to L2, with suggested fix |
| otherwise (novel)                                                                    | Escalate to category expert  |

Auto-resolution is opt-in per fix: a human must mark a resolution
`automatable`. Fixes learned from Jira are never automatable by default,
and echoes of the service's own Jira transitions are ignored, so the system
never learns from itself.

### Retrieval (the "R" in RAG)

`Embedder` turns text into vectors and `KnowledgeBase` finds nearest
neighbours by cosine similarity.

* Today: `HashingEmbedder`, an offline, deterministic, lexical baseline with
  no external dependency, plus a brute-force scan in SQLite. This is fine to
  roughly 10^4 entries.
* Next: a neural embedder (an HTTP embedding API or a local ONNX model) in
  `ticket-retrieval` behind a cargo feature, and a vector index (`sqlite-vec`,
  Postgres with `pgvector`, or Qdrant) behind `KnowledgeBase`.
  `Embedder::model_id` exists so stored vectors can be re-indexed when the
  model changes.
* Later: **generation**. An LLM-backed `Classifier`, and a step that drafts
  the reply or fix from the retrieved tickets, both plugging into existing
  ports with the rule-based path kept as a fallback.

## Multi-client design

* Every ticket carries a `ClientId` (tenant).
* Every external ticket carries an `ExternalRef { system, key }`. `system`
  is the **connector instance name** (for example `jira-acme`), and the
  `Dispatcher` uses it to pick the `Tracker` that writes back.
* Connectors are configured as a map (`[connectors.jira.<name>]`). Two
  customers on different Jira sites are two entries. Each gets its own
  webhook URL, secret and credentials.
* **Adding a new tracker** (ServiceNow, Zendesk, GitHub Issues) means a new
  `crates/connectors/<name>` crate with a config struct, a webhook parser
  producing `NewTicket`, and a `Tracker` impl. Then one route and a few
  lines in `bootstrap.rs`. Nothing in core or the engine changes.
* **Standalone clients** use the REST API directly, or the `ticket-client`
  SDK, which shares its request/response types with the server through
  `ticket-api`, so the two cannot drift.

## Security baseline

* Optional bearer token on `/v1` (compared in constant time).
* Jira webhooks are verified with HMAC-SHA256 (`X-Hub-Signature`) over the raw body.
* 1 MiB request body limit, and input length validation on tickets.
* Secrets are redacted from `Debug` output and can be supplied via environment variables.
* 5xx responses never leak internal error text. It is logged instead.
* Jira issue keys are validated before being placed in request paths.

## Testing strategy

| Layer               | Tests                                                                 |
|---------------------|-----------------------------------------------------------------------|
| Domain              | Unit tests on invariants, parsing and serialisation                   |
| Router / classifier | Exhaustive unit tests (pure functions)                                |
| Storage             | One **contract suite** run against every adapter (SQLite, in-memory)  |
| Engine              | End-to-end pipeline with real adapters; only the tracker is recorded  |
| HTTP                | Real server on an ephemeral port, driven through the client SDK       |
| Jira                | Real webhook payloads, real HMAC signatures, ADF round-trips          |

## Roadmap

1. **Durable queue / outbox**: replace the in-process channel with a
   DB-backed outbox or a broker, so several replicas can run.
   Claim expiry would use `updated_at`.
2. **Retry policy**: backoff for `failed` tickets, plus a
   `POST /v1/tickets/{id}/retriage` endpoint.
3. **Postgres adapter** with `pgvector`, run against the existing contract suite.
4. **Neural embeddings** and an **LLM classifier and reply drafting**.
5. **Notifiers**: Slack, email, PagerDuty for critical escalations.
6. **More connectors**: ServiceNow, Zendesk, GitHub Issues.
7. **Per-client policy**: thresholds, rules and escalation directories keyed by `ClientId`.
8. **Observability**: OpenTelemetry traces, Prometheus metrics (auto-resolve
   rate, time to triage, escalation mix).
