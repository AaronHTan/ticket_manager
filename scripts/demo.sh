#!/usr/bin/env bash
# Starts the browser demo: a ticket-handler server with a seeded knowledge
# base and a local git repository it can fix. Two pages: ticket intake (/)
# and the escalation inbox (/inbox).
#
# Usage: scripts/demo.sh [--auto] [--with-tickets] [--no-open] [--port N]
#
#   --auto           start in automatic mode, as production runs (the page
#                    can switch modes either way at any time)
#   --with-tickets   also load the 10 sample tickets (already triaged)
#   --no-open        do not open a browser window
#   --port N         listen on port N instead of 18081
#
# Everything lives under data/demo/ and is recreated on every run.
# Press Ctrl+C to stop. See docs/DEMO.md for a walkthrough.

set -euo pipefail
cd "$(dirname "$0")/.."

AUTO=false
WITH_TICKETS=0
OPEN_BROWSER=1
PORT=18081
while [[ $# -gt 0 ]]; do
    case "$1" in
        --with-tickets) WITH_TICKETS=1 ;;
        --no-open) OPEN_BROWSER=0 ;;
        --port) PORT="${2:?--port needs a value}"; shift ;;
        --auto) AUTO=true ;;
        -h|--help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    esac
    shift
done

DEMO=data/demo
REMOTE="$DEMO/remotes/web-app.git"
BIN=target/debug
URL="http://127.0.0.1:$PORT"
unset TICKET_HANDLER_TOKEN TICKET_HANDLER_URL TICKET_HANDLER_CONFIG

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
need() { command -v "$1" >/dev/null || { echo "error: '$1' is required but not installed" >&2; exit 1; }; }
need cargo
need git
need curl

say "Building (the first build takes a minute or two)"
cargo build --quiet -p ticket-server -p ticket-cli

say "Creating the demo repository $REMOTE from demo/repo"
rm -rf "$DEMO"
mkdir -p "$DEMO/remotes"
git init --quiet --bare --initial-branch=main "$REMOTE"
work=$(mktemp -d)
cp -R demo/repo/. "$work"
git -C "$work" init --quiet --initial-branch=main
git -C "$work" add --all
git -C "$work" -c user.name=demo -c user.email=demo@example.test commit --quiet -m "Initial shop"
git -C "$work" push --quiet "$PWD/$REMOTE" main
rm -rf "$work"

say "Seeding the knowledge base ($DEMO/demo.db)"
if [[ $WITH_TICKETS -eq 1 ]]; then
    "$BIN/ticket-seed" --config config/demo.toml --tickets demo/tickets.json --reset | sed -n '1,4p'
else
    "$BIN/ticket-seed" --config config/demo.toml --no-tickets --reset
fi

say "Starting the server on $URL"
TICKET_HANDLER_CONFIG=config/demo.toml TICKET_HANDLER_SERVER__BIND="127.0.0.1:$PORT" \
    TICKET_HANDLER_SERVER__AUTO_TRIAGE="$AUTO" \
    "$BIN/ticket-server" >"$DEMO/server.log" 2>&1 &
SERVER_PID=$!
stop_server() {
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
}
trap stop_server EXIT
trap 'echo; say "Stopping"; exit 0' INT TERM
for _ in $(seq 1 100); do
    curl -sf "$URL/healthz" >/dev/null && break
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        echo "error: the server exited; its log follows" >&2
        cat "$DEMO/server.log" >&2
        exit 1
    fi
    sleep 0.1
done

if [[ $OPEN_BROWSER -eq 1 ]]; then
    if command -v open >/dev/null; then open "$URL"
    elif command -v xdg-open >/dev/null; then xdg-open "$URL" >/dev/null 2>&1 || true
    fi
fi

if [[ $AUTO == true ]]; then MODE="automatic"; else MODE="step-by-step"; fi
cat <<EOF

  Demo running ($MODE mode; switch at the top of the page)
    Ticket intake      $URL/
    Escalation inbox   $URL/inbox

  Step-by-step: create a ticket, then click Run triage, Attempt code fix
  and Record resolution to see each stage.

  Automatic (production): create tickets, or click "Simulate incoming
  tickets", and everything runs on its own. Open the escalation inbox in a
  second window to watch severe tickets arrive with a drafted response.

  Fix branches: git --git-dir=$REMOTE branch --list 'ticket/*'
  Server log:   $DEMO/server.log

  Press Ctrl+C to stop.
EOF

wait "$SERVER_PID"
