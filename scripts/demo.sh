#!/usr/bin/env bash
# End-to-end demo: tickets are triaged against a seeded SQLite database, and
# a known bug is fixed automatically by re-applying the patch of a resolved
# ticket, then pushed to a branch in a local git repository.
#
# Usage: scripts/demo.sh [--keep-running]
#
# Everything is created under data/demo/ and removed at the start of each run.
# See docs/DEMO.md for a walkthrough.

set -euo pipefail
cd "$(dirname "$0")/.."

KEEP_RUNNING=0
case "${1:-}" in
    --keep-running) KEEP_RUNNING=1 ;;
    "") ;;
    *) echo "usage: $0 [--keep-running]" >&2; exit 2 ;;
esac

DEMO=data/demo
REMOTE="$DEMO/remotes/web-app.git"
BIN=target/debug
export TICKET_HANDLER_URL=http://127.0.0.1:18081
unset TICKET_HANDLER_TOKEN TICKET_HANDLER_CONFIG

step() { printf '\n\033[1;34m== %s\033[0m\n' "$*"; }
note() { printf '\033[2m%s\033[0m\n' "$*"; }
need() { command -v "$1" >/dev/null || { echo "error: '$1' is required but not installed" >&2; exit 1; }; }
remote_git() { git --git-dir="$REMOTE" "$@"; }

need cargo
need git
need curl

step "1/8  Build the server, CLI and seeder"
cargo build --quiet -p ticket-server -p ticket-cli
note "built $BIN/ticket-server, $BIN/ticket-seed and $BIN/ticket"

step "2/8  Create the demo repository (a tiny shop with a bug in cart.py)"
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
note "bare repository $REMOTE, branch main:"
remote_git log --oneline main
note "the bug, in cart.py on main:"
remote_git show main:cart.py | grep -n "for item in items"

step "3/8  Seed the database: 20 resolved tickets, 10 incoming tickets triaged"
note "one resolved ticket (\"Checkout returns 500 on empty cart\") stored the patch that fixed it"
"$BIN/ticket-seed" --config config/demo.toml --tickets demo/tickets.json --reset

step "4/8  Start the server on the demo database"
TICKET_HANDLER_CONFIG=config/demo.toml "$BIN/ticket-server" >"$DEMO/server.log" 2>&1 &
SERVER_PID=$!
stop_server() { kill "$SERVER_PID" 2>/dev/null || true; wait "$SERVER_PID" 2>/dev/null || true; }
trap stop_server EXIT
for _ in $(seq 1 100); do
    curl -sf "$TICKET_HANDLER_URL/healthz" >/dev/null && break
    kill -0 "$SERVER_PID" 2>/dev/null || { cat "$DEMO/server.log" >&2; exit 1; }
    sleep 0.1
done
"$BIN/ticket" health
note "on start-up the server re-queues the ticket left 'received' (FX-10):"
sleep 1
grep -E "re-queueing|ticket triaged" "$DEMO/server.log" | sed -E 's/\x1b\[[0-9;]*m//g; s/^[^ ]+ +//' || true

step "5/8  Everything in the database (ticket list)"
"$BIN/ticket" list

step "6/8  The fix proposed for FX-9 (\"Checkout crashes with 500 when cart is empty\")"
fx9=$("$BIN/ticket" list | awk '/Checkout crashes with 500/ {print $1; exit}')
branch="ticket/$fx9"
note "triage forwarded it to L2, and the fix path pushed branch $branch:"
remote_git log -1 --format='%h  %an%n    %s%n%n%b' "$branch"
remote_git diff main "$branch"

step "7/8  Does the fix work? Run the shop's tests on main and on the fix branch"
if command -v python3 >/dev/null; then
    for ref in main "$branch"; do
        tree=$(mktemp -d)
        remote_git archive "$ref" | tar -x -C "$tree"
        echo "$ref:"
        (cd "$tree" && python3 - <<'PY'
import test_cart

for name in ("test_total", "test_checkout_with_items", "test_checkout_with_empty_cart"):
    try:
        getattr(test_cart, name)()
        print(f"  pass  {name}")
    except Exception as error:
        print(f"  FAIL  {name}: {type(error).__name__}: {error}")
PY
        )
        rm -rf "$tree"
    done
else
    note "python3 not found; skipping (the diff above is the whole fix)"
fi

step "8/8  A new ticket arrives over the API"
note "ticket submit --client acme --kind bug --component checkout --title \"Checkout returns 500 on empty cart\" ..."
new_id=$("$BIN/ticket" submit --client acme --kind bug --component checkout --repo web-app \
    --reporter "dana@acme.test" \
    --title "Checkout returns 500 on empty cart" \
    --description "Submitting checkout with an empty cart crashes with a 500 error and a stack trace." \
    | awk -F'"' '/"id"/ {print $4}')
note "accepted as $new_id; waiting for triage and the fix attempt..."
fix=""
for _ in $(seq 1 100); do
    fix=$("$BIN/ticket" list --limit 20 | awk -v id="$new_id" '$1 == id {print $4}')
    [[ -n "$fix" && "$fix" != "-" ]] && break
    sleep 0.1
done
"$BIN/ticket" list --limit 1
note "its fix attempt (from ticket get $new_id):"
"$BIN/ticket" get "$new_id" | sed -n '/"fix": {/,$p'
note "branches on the remote now:"
remote_git branch --list 'ticket/*'

printf '\n\033[1;32mDemo complete.\033[0m\n'
cat <<EOF
  database      $DEMO/demo.db        (sqlite3 $DEMO/demo.db "select status, count(*) from tickets group by status")
  repository    $REMOTE   (git --git-dir=$REMOTE log --all --oneline)
  server log    $DEMO/server.log
EOF

if [[ $KEEP_RUNNING -eq 1 ]]; then
    cat <<EOF

The server is still running at $TICKET_HANDLER_URL (Ctrl+C to stop). Try:
  export TICKET_HANDLER_URL=$TICKET_HANDLER_URL
  $BIN/ticket list --status forwarded
  $BIN/ticket get $new_id
EOF
    wait "$SERVER_PID"
fi
