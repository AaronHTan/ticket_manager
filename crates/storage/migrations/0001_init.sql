-- Live tickets received from any client.
CREATE TABLE tickets (
    id                TEXT PRIMARY KEY NOT NULL,
    client_id         TEXT NOT NULL,
    external_system   TEXT,
    external_key      TEXT,
    title             TEXT NOT NULL,
    description       TEXT NOT NULL,
    reporter          TEXT,
    labels            TEXT NOT NULL DEFAULT '[]',   -- JSON array of strings
    reported_severity TEXT,
    status            TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    UNIQUE (external_system, external_key)
);

CREATE INDEX idx_tickets_status_created ON tickets (status, created_at);
CREATE INDEX idx_tickets_client ON tickets (client_id);

-- Append-only audit log of triage decisions.
CREATE TABLE triage_reports (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ticket_id  TEXT NOT NULL REFERENCES tickets (id) ON DELETE CASCADE,
    report     TEXT NOT NULL,                     -- JSON-serialised TriageReport
    decided_at TEXT NOT NULL
);

CREATE INDEX idx_triage_reports_ticket ON triage_reports (ticket_id, id);

-- Resolved tickets used for similarity search. ticket_id is not a foreign key
-- because imported history has no row in `tickets`.
CREATE TABLE knowledge (
    ticket_id          TEXT PRIMARY KEY NOT NULL,
    title              TEXT NOT NULL,
    description        TEXT NOT NULL,
    category           TEXT,
    severity           TEXT NOT NULL,
    resolution_summary TEXT NOT NULL,
    automatable        INTEGER NOT NULL,
    resolved_by        TEXT,
    embedding_dim      INTEGER NOT NULL,
    embedding          BLOB NOT NULL,             -- little-endian f32 array
    updated_at         TEXT NOT NULL
);
