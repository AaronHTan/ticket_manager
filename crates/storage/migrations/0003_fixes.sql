-- Code fixes: patches learned from resolutions, and fix attempts per ticket.

-- Unified diff that fixed the original ticket, if any.
ALTER TABLE knowledge ADD COLUMN resolution_patch TEXT;

-- Append-only log of fix attempts.
CREATE TABLE fix_attempts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ticket_id    TEXT NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    attempt      TEXT NOT NULL,                   -- JSON-serialised FixAttempt
    attempted_at TEXT NOT NULL
);

CREATE INDEX idx_fix_attempts_ticket ON fix_attempts (ticket_id, id);
