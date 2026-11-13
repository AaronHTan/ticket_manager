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

**Try it:** `./scripts/demo.sh` starts the service with a seeded knowledge
base and opens a web page where you create a ticket and step it through
triage, an automatic code fix (pushed to a local git repository) and
resolution. [docs/DEMO.md](docs/DEMO.md) walks through it.

## Requirements

* Rust **1.99.1** (pinned in `rust-toolchain.toml`; `rustup` installs it automatically), edition 2024.
* No external services. SQLite is bundled.
* `git` on `PATH`, but only when `[repositories]` are configured.

## Quick start

```bash
cargo run -p ticket-server                     # listens on 127.0.0.1:8080, data in ./data
```

In another terminal:

```bash
cargo run -p ticket-cli -- import examples/history.json
cargo run -p ticket-cli -- submit --client acme \
    --title "Password reset email not received" \
    --description "User requested a password reset but the reset email never arrived." \
    --kind bug --component auth --reporter jane@acme.test
cargo run -p ticket-cli -- list --status escalated
cargo run -p ticket-cli -- get <id-from-previous-command>
cargo run -p ticket-cli -- resolve <id> --summary "What fixed it" --automatable
```

The CLI binary is called `ticket`. It reads `TICKET_HANDLER_URL` and `TICKET_HANDLER_TOKEN`.

## HTTP API (v1)

| Method | Path                               | Auth           | Purpose                              |
|--------|------------------------------------|----------------|--------------------------------------|
| GET    | `/`                                | none           | Demo web UI (static, no data)        |
| GET    | `/healthz`                         | none           | Liveness, mode and knowledge-base size |
| POST   | `/v1/tickets`                      | bearer token   | Submit a ticket (`202 Accepted`)     |
| GET    | `/v1/tickets?status=&client=&limit=` | bearer token | List tickets, newest first         |
| GET    | `/v1/tickets/{id}`                 | bearer token   | Ticket, triage report and fix attempt |
| POST   | `/v1/tickets/{id}/triage`          | bearer token   | Triage now (step-by-step mode)       |
| POST   | `/v1/tickets/{id}/fix`             | bearer token   | Attempt a code fix now (409 says why not) |
| POST   | `/v1/tickets/{id}/resolution`      | bearer token   | Record a human fix (learning loop)   |
| POST   | `/v1/knowledge/import`             | bearer token   | Seed the knowledge base              |
| POST   | `/v1/webhooks/jira/{connector}`    | HMAC signature | Jira webhook receiver                |

Bearer auth is enabled by setting `server.api_token`.

A submitted ticket needs only `client` and `title`. It can also carry
`description`, `reporter` (a string, or `{name, email, external_id}`), `kind`
(`bug`, `incident`, `feature_request`, `task`, `question`, `other`),
`labels`, `components`, `severity`, `code_context` (`{repository, git_ref,
paths, version}`), `links` (`[{url, title}]`), `reported_at`, `metadata`
(string map) and `external_ref`.

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
with the same secret. The Jira issue type, components, reporter, creation
time and an optional `repository_field` custom field are mapped onto the
ticket.

## Running against a test database

`config/test.toml` points at `data/test.db` and port 18080. `ticket-seed`
fills that database from `fixtures/`: 20 resolved tickets go into the
knowledge base, and 10 incoming tickets are triaged through the real
pipeline. Each fixture ticket records the status it should reach, so the
seeder doubles as a routing regression check and exits non-zero on any
mismatch.

```bash
cargo run -p ticket-server --bin ticket-seed -- --reset    # rebuild data/test.db from fixtures
TICKET_HANDLER_CONFIG=config/test.toml cargo run -p ticket-server
TICKET_HANDLER_URL=http://127.0.0.1:18080 cargo run -p ticket-cli -- health
sqlite3 data/test.db "select status, count(*) from tickets group by status"
```

Re-running without `--reset` is a no-op. History is only imported into an
empty knowledge base, and tickets are deduplicated by `external_ref`.
`--reset` only deletes SQLite files whose name contains `test`, unless you
pass `--force`. One fixture is left `received`, so starting the server
shows recovery re-queuing and triaging it.

## Code changes

Repositories the service may change are listed under `[repositories.<id>]`.
With `[fixes] enabled = true`, triage hands eligible tickets (bugs and
incidents that were forwarded or escalated) to `FixCoordinator`. It creates a
throwaway git worktree on branch `ticket/<id>`, re-applies the patch from the
most similar resolved ticket, then commits and pushes the branch. The code
host only logs for now, so review happens on the branch. See
[docs/DEMO.md](docs/DEMO.md) to watch this end to end, and *Code changes* in
the architecture doc for details.

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
crates/vcs/git           SourceControl adapter over the git CLI (bare clone + worktrees)
apps/server              axum service (composition root) + `ticket-seed`
apps/cli                 `ticket` command-line client
config/test.toml         configuration for the seeded test database
fixtures/                history + incoming tickets with expected routes (used by ticket-seed and tests)
demo/                    demo shop repository (with a planted bug) and demo ticket set
config/demo.toml         configuration for the demo (step-by-step, fixes on, local repository)
scripts/demo.sh          starts the browser demo
apps/server/ui/          the demo web UI (plain HTML/CSS/JS, compiled into the server)
```
