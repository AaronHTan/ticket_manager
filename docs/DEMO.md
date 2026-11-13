# Demo: from ticket to fix branch

This demo runs the whole service on your machine against a seeded SQLite test
database and a local git repository. It shows tickets being triaged
(auto-resolved, forwarded or escalated), and a known bug being **fixed
automatically**: the service finds a similar resolved ticket, re-applies the
patch that fixed it, and pushes the change to a review branch.

Nothing leaves your machine. Everything is created under `data/demo/`.

## Prerequisites

| Tool    | Why                              | Check              |
|---------|----------------------------------|--------------------|
| rustup  | builds the project. The pinned toolchain (1.99.1) installs itself on first build | `cargo --version` |
| git     | the fix path drives `git`        | `git --version`    |
| curl    | the script waits for the server  | `curl --version`   |
| python3 | *optional*: runs the demo shop's tests to prove the fix | `python3 --version` |
| sqlite3 | *optional*: inspect the database | `sqlite3 --version` |

## Run it (one command)

From the repository root:

```bash
./scripts/demo.sh
```

The first run compiles the workspace, which takes a minute or two. Later runs
take a few seconds. The script stops the server when it finishes. To keep the
server running so you can explore, use:

```bash
./scripts/demo.sh --keep-running
```

## What the steps show

1. **Build**: `ticket-server`, `ticket-seed` and the `ticket` CLI.
2. **Demo repository**: `demo/repo` (a tiny shop) is pushed to a bare git
   repository at `data/demo/remotes/web-app.git`. `cart.py` has a planted
   bug: checking out a cart with no items crashes with
   `TypeError: 'NoneType' object is not iterable`.
3. **Seed**: `ticket-seed` loads 20 resolved tickets into the knowledge
   base. One of them, *"Checkout returns 500 on empty cart"*, stored the
   patch that fixed it. It then submits and triages 10 incoming tickets
   (`demo/tickets.json`) and checks each ended where expected:
   * FX-1 and FX-2 are near-duplicates of automatable fixes and are **auto-resolved**.
   * FX-3, FX-4, FX-8 and FX-9 are known issues and are **forwarded** to L2.
   * FX-5, FX-6 and FX-7 are a security issue, a novel request and a critical
     outage, so they are **escalated**.
   * FX-9, *"Checkout crashes with 500 when cart is empty"*, is a bug in the
     `web-app` repository. The fix path re-applies the stored patch and
     pushes branch `ticket/<id>`, shown as `fix = proposed`.
   * FX-3 and FX-4 were also eligible, but no stored patch is similar enough,
     so they record `no_changes` and stay with a human.
   * FX-10 is left `received` on purpose.
4. **Server**: starts on `127.0.0.1:18081`. On start-up it re-queues FX-10,
   just as it would recover tickets after a crash.
5. **`ticket list`**: every ticket with its status, kind and fix result.
6. **The fix for FX-9**: the commit message, which names the precedent and its
   similarity, and the diff against `main`.
7. **Proof**: the shop's tests run on `main` (the empty-cart test fails) and
   on the fix branch (all pass).
8. **A live ticket**: a new checkout-crash ticket is submitted over the HTTP
   API. Within a moment it is triaged, gets its own fix branch, and
   `ticket get` shows the recorded fix attempt.

## The same steps by hand

Run these from the repository root. Each one is what the script does.

```bash
# 1. Build
cargo build -p ticket-server -p ticket-cli

# 2. Create the demo repository
rm -rf data/demo && mkdir -p data/demo/remotes
git init --bare --initial-branch=main data/demo/remotes/web-app.git
work=$(mktemp -d) && cp -R demo/repo/. "$work"
git -C "$work" init --initial-branch=main
git -C "$work" add --all
git -C "$work" -c user.name=demo -c user.email=demo@example.test commit -m "Initial shop"
git -C "$work" push "$PWD/data/demo/remotes/web-app.git" main

# 3. Seed the demo database (prints the routing and fix table)
./target/debug/ticket-seed --config config/demo.toml --tickets demo/tickets.json --reset

# 4. Start the server (leave it running in this terminal)
TICKET_HANDLER_CONFIG=config/demo.toml ./target/debug/ticket-server
```

In a second terminal:

```bash
export TICKET_HANDLER_URL=http://127.0.0.1:18081

# 5. Browse the tickets
./target/debug/ticket list
./target/debug/ticket list --status forwarded

# 6. Look at a fix branch
git --git-dir=data/demo/remotes/web-app.git branch --list 'ticket/*'
git --git-dir=data/demo/remotes/web-app.git diff main ticket/<id>

# 8. Submit a ticket yourself and watch it get fixed
./target/debug/ticket submit --client acme --kind bug --component checkout \
    --title "Checkout returns 500 on empty cart" \
    --description "Submitting checkout with an empty cart crashes with a 500 error."
./target/debug/ticket get <id-from-the-previous-command>
```

## Things to try

* **Teach it a new fix.** Resolve a forwarded ticket with a patch, then submit a
  similar ticket. This works when the patch applies to `main`:
  ```bash
  ./target/debug/ticket resolve <id> --summary "What fixed it" --patch-file fix.patch
  ```
* **See what is not fixed.** Submit a `--kind question`, or a ticket mentioning a
  "security breach" (critical severity). Neither gets a fix attempt.
* **Inspect the database:**
  `sqlite3 data/demo/demo.db "select status, count(*) from tickets group by status"`.
* **Read the server log:** `data/demo/server.log`.

## Reset

`./scripts/demo.sh` starts from scratch every time. To remove everything, run
`rm -rf data/demo`.

## Configuration used

`config/demo.toml` is `config/test.toml` plus three additions:

```toml
[repositories.web-app]
remote = "data/demo/remotes/web-app.git"
clients = ["acme"]
components = ["checkout", "cart"]

[fixes]
enabled = true   # bugs and incidents that were forwarded or escalated, up to high severity
```
