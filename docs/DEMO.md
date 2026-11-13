# Demo: from ticket to fix branch, in the browser

The demo runs the service on your machine with a seeded knowledge base and a
local git repository. It has two pages:

* **Ticket intake** (`/`): create tickets and follow them through the pipeline.
* **Escalation inbox** (`/inbox`): what an expert team sees. Severe or novel
  tickets arrive there live, each with a drafted preliminary resolution.

It runs in two modes, switched with the toggle at the top of either page:

* **Step-by-step** (the default): you run each stage yourself, to see what
  the system does at each point.
* **Automatic**: how it runs in production. You only create tickets, and
  triage, code fixes and escalations happen on their own.

In step-by-step mode, a ticket goes through four stages:

1. **Create**: write a ticket, or start from one of the examples.
2. **Triage**: see its severity and the reasons for it, the most similar
   resolved tickets, and the routing decision (auto-resolve, forward to L2,
   or escalate to an expert).
3. **Code fix**: for an eligible bug or incident, the service re-applies the
   patch of the most similar resolved ticket in a throwaway checkout and
   pushes it to a review branch. The page shows the diff.
4. **Resolve**: record what really fixed it. The knowledge base learns, so
   the next similar ticket is handled better.

Nothing leaves your machine. Everything is created under `data/demo/`.

## Prerequisites

| Tool    | Why                                  | Check            |
|---------|--------------------------------------|------------------|
| rustup  | builds the project. The pinned toolchain (1.99.1) installs itself on first build | `cargo --version` |
| git     | the fix step drives `git`            | `git --version`  |
| curl    | the script waits for the server      | `curl --version` |
| A browser | the demo UI                        |                  |

## Run it

From the repository root:

```bash
./scripts/demo.sh
```

The script:

1. builds the server, CLI and seeder (the first build takes a minute or two);
2. creates the demo repository `data/demo/remotes/web-app.git` from
   `demo/repo`. That is a tiny shop whose `cart.py` crashes when a cart has
   no items;
3. seeds `data/demo/demo.db` with 20 resolved tickets. One of them, *"Checkout
   returns 500 on empty cart"*, stored the patch that fixed it;
4. starts the server on **http://127.0.0.1:18081** in *step-by-step mode* and
   opens it in your browser.

It runs until you press **Ctrl+C**, and starts from scratch each time.

| Option           | Effect                                                        |
|------------------|---------------------------------------------------------------|
| `--auto`         | start in automatic mode (the toggle still works)              |
| `--with-tickets` | also load the 10 sample tickets, already triaged              |
| `--no-open`      | do not open a browser window                                  |
| `--port N`       | listen on port `N` instead of 18081                           |

## Walkthrough: step-by-step mode

### 1. Create a ticket

The left panel has a form and a list of tickets. Pick an example from **Start
from an example**. Each one comes with a note on what will happen to it.

| Example                        | What happens                                                    |
|--------------------------------|-----------------------------------------------------------------|
| Checkout crash on empty cart   | Forwarded to L2, then gets a **code fix** on a review branch    |
| Password reset email missing   | **Auto-resolved**: a known fix that is safe to apply            |
| Duplicate subscription charge  | Forwarded. The fix step finds no patch close enough and changes nothing |
| Suspicious admin logins        | Critical, so it is **escalated** to Security and never auto-fixed |
| Dark mode request              | Novel, so it is **escalated** to an expert. Good for trying the learning loop (below) |
| Production outage              | Critical, so it is **escalated** to on-call at once, with a drafted incident response |

You can also edit any field, or write your own ticket. Click **Create
ticket**. It appears in the list as *Received* and opens on the right.

### 2. Run triage

Click **Run triage**. The step shows:

* **Severity**, with confidence and the signals behind it (keywords, the
  reported severity, similar past tickets);
* **Most similar resolved tickets**, with similarity bars;
* **The decision**: who handles it and why, plus the suggested fix from the
  closest match.

### 3. Attempt a code fix

Click **Attempt code fix**. For the checkout example, the step shows *Fix
proposed*, the branch and commit, and the diff. The diff is the one-line guard
that makes an empty cart total 0.

If a ticket is not eligible (a question, a critical issue, an auto-resolved
ticket), the step says why instead. If no stored patch is similar enough, it
records *No fix* and leaves the ticket to a human.

To check the branch from a terminal:

```bash
git --git-dir=data/demo/remotes/web-app.git branch --list 'ticket/*'
git --git-dir=data/demo/remotes/web-app.git diff main ticket/<ticket-id>
```

### 4. Resolve

Record what fixed it. If a fix was proposed, the form is pre-filled and
**Store the proposed patch with this resolution** is ticked. The ticket
becomes *Resolved*, and the resolution joins the knowledge base.

**See the learning loop.** Create the *Dark mode request* example and triage it.
It is escalated as novel. Resolve it with an answer and *Safe to apply
automatically* ticked. Then create the same ticket again and triage it. This
time it is **auto-resolved** with your answer.

The checkout crash will not auto-resolve this way, by design. It is assessed
as *high* severity, and nothing above medium is auto-resolved however well it
matches. A second checkout ticket is forwarded with "severity high requires
human review", and gets its own code fix.

## Walkthrough: automatic mode (production)

1. Click **Automatic** at the top of the page, or start with
   `./scripts/demo.sh --auto`. Any ticket still waiting is picked up straight away.
2. Open the **Escalation inbox** in a second window, side by side, so you can
   watch tickets arrive.
3. On the intake page, create tickets, or click **Simulate incoming tickets**,
   which sends all six examples a moment apart. Each one goes through on its own:
   * the password reset is **auto-resolved**;
   * the checkout crash is **forwarded**, and its **code fix** is pushed to a
     branch within a second or two;
   * the duplicate charge is **forwarded**, and its fix attempt finds nothing
     safe to apply;
   * the admin-login breach and the outage are **critical**, and the dark-mode
     request is **novel**, so all three are **escalated**. They appear in the
     inbox, marked *New*, under Security Team or Platform On-Call.

### The escalation inbox

Each escalated ticket shows its severity, team, why it was escalated, and a
**preliminary resolution**:

* **Suggested first steps** from a playbook for the problem category
  (security, database, billing, authentication, performance) or the kind of
  request (feature requests and questions get their own). Critical tickets
  start with an incident step.
* **Similar past cases**: the closest resolved tickets and how they were
  fixed, even when they are not close enough to forward on. If nothing is
  close, the draft says the ticket looks new.

Filter by **Team** to see one team's queue. Open **Review and send the
resolution** to edit the draft and send it. The ticket leaves the inbox as
*Resolved*, and its resolution joins the knowledge base. The *Escalation
inbox* tab shows a count of waiting escalations on both pages.

The draft is never sent on its own: it is a starting point for the expert.
When a ticket came from Jira, the draft is also added to the escalation
comment on the issue.

## The same steps from a terminal

The page uses the same HTTP API as the `ticket` CLI, so you can drive the
demo from a second terminal while it runs:

```bash
export TICKET_HANDLER_URL=http://127.0.0.1:18081
T=./target/debug/ticket

$T submit --client acme --kind bug --component checkout --repo web-app \
    --title "Checkout returns 500 on empty cart" \
    --description "Submitting checkout with an empty cart crashes with a 500 error."
$T list
$T triage <id>     # step 2
$T fix <id>        # step 3
$T resolve <id> --summary "Merged the automatic fix"   # step 4
```

The page refreshes every few seconds, so tickets created this way appear there too.

## How the modes work

`config/demo.toml` sets:

```toml
[server]
auto_triage = false        # start step-by-step: tickets wait in `received`
allow_mode_switch = true   # let the page switch modes (PUT /v1/settings); keep off in production

[repositories.web-app]
remote = "data/demo/remotes/web-app.git"
clients = ["acme"]
components = ["checkout", "cart"]

[fixes]
enabled = true        # bugs and incidents that were forwarded or escalated, up to high severity
```

With `auto_triage = false`, the server never triages on its own. The buttons
call `POST /v1/tickets/{id}/triage` and `POST /v1/tickets/{id}/fix`.
Switching to automatic (`PUT /v1/settings {"auto_triage": true}`) queues
anything waiting, and from then on every ticket is triaged by the worker
pool as it arrives, exactly as in production. A production server keeps
`allow_mode_switch` off, so its mode cannot be changed over the API.

## Troubleshooting

* **Port in use**: run `./scripts/demo.sh --port 18090`.
* **Server failed to start**: the script prints `data/demo/server.log`.
* **A token prompt appears**: the server was started with `server.api_token`.
  The demo config does not set one, so check `TICKET_HANDLER_SERVER__API_TOKEN`
  in your environment.
* **Reset**: rerun the script, or `rm -rf data/demo`.
