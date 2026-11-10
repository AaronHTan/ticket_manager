# ticket-handler

A Rust service that receives issue tickets from external clients (Jira or the
standalone API), scores their severity, compares them against a knowledge
base of previously resolved tickets, and then does one of three things:

| Outcome          | When                                                               | Who handles it               |
|------------------|--------------------------------------------------------------------|------------------------------|
| **Auto-resolve** | Near-duplicate of a past ticket whose fix is marked *automatable*  | The service, without a human |
| **Forward**      | Similar to a known issue, but needs a human to apply the fix       | Intermediate (L2) support    |
| **Escalate**     | Novel, or severity at or above the escalation threshold            | Category expert              |

Human resolutions are fed back into the knowledge base, so over time more
tickets fall into the first two buckets.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design.

## Requirements

* Rust **1.99.1** (pinned in `rust-toolchain.toml`; `rustup` installs it automatically), edition 2024.
* No external services. SQLite is bundled.

## Quick start

```bash
cargo run -p ticket-server                     # listens on 127.0.0.1:8080, data in ./data
```

In another terminal:

```bash
cargo run -p ticket-cli -- import examples/history.json
cargo run -p ticket-cli -- submit --client acme \
    --title "Password reset email not received" \
    --description "User requested a password reset but the reset email never arrived."
cargo run -p ticket-cli -- get <id-from-previous-command>
cargo run -p ticket-cli -- resolve <id> --summary "What fixed it" --automatable
```

The CLI binary is called `ticket`. It reads `TICKET_HANDLER_URL` and `TICKET_HANDLER_TOKEN`.

## HTTP API (v1)

| Method | Path                               | Auth           | Purpose                              |
|--------|------------------------------------|----------------|--------------------------------------|
| GET    | `/healthz`                         | none           | Liveness and knowledge-base size     |
| POST   | `/v1/tickets`                      | bearer token   | Submit a ticket (`202 Accepted`)     |
| GET    | `/v1/tickets/{id}`                 | bearer token   | Ticket and latest triage report      |
| POST   | `/v1/tickets/{id}/resolution`      | bearer token   | Record a human fix (learning loop)   |
| POST   | `/v1/knowledge/import`             | bearer token   | Seed the knowledge base              |
| POST   | `/v1/webhooks/jira/{connector}`    | HMAC signature | Jira webhook receiver                |

Bearer auth is enabled by setting `server.api_token`.

## Configuration

`config/default.toml` documents every option. Override any key with an
environment variable: `TICKET_HANDLER_` plus the path, with `__` between levels.

```bash
TICKET_HANDLER_SERVER__API_TOKEN=change-me \
TICKET_HANDLER_DATABASE__URL=memory \
cargo run -p ticket-server
```

To connect a Jira site, add a `[connectors.jira.<name>]` table (see the
commented example in the config), then point a Jira webhook for *issue
created* and *issue updated* at `https://<host>/v1/webhooks/jira/<name>`
with the same secret.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

## Workspace layout

```
crates/core              domain types + port traits (no I/O)
crates/engine            classifier, router, dispatcher, pipeline, worker pool
crates/retrieval         embedders (offline hashing embedder today)
crates/storage           SQLite + in-memory adapters, shared contract tests
crates/api               HTTP request/response types (shared by server and SDK)
crates/client            async Rust SDK for the HTTP API
crates/connectors/jira   Jira Cloud webhook parser + Tracker implementation
apps/server              axum service (composition root)
apps/cli                 `ticket` command-line client
```
