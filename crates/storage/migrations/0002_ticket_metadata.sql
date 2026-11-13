-- Richer ticket metadata.
--
-- `reporter` keeps its column but now holds a JSON-serialised Reporter.
-- Rows written before this migration hold a plain label, which the adapter
-- still reads.
ALTER TABLE tickets ADD COLUMN kind TEXT NOT NULL DEFAULT 'other';
ALTER TABLE tickets ADD COLUMN reported_at TEXT;
-- JSON object: components, code_context, links, metadata.
ALTER TABLE tickets ADD COLUMN details TEXT NOT NULL DEFAULT '{}';
