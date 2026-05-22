-- M5.1 — local query history + saved-query snippets.
--
-- Both tables hold Quill's own metadata, never the user's Postgres data.
--
-- query_history: one row per executed query (success or failure).
--   `row_count` is intentionally absent; see tasks/m5-1-history-saved-store.md
--   for the rationale.  `duration_ms` is the time-to-first-chunk measured
--   inside `query::run_query` and reflects what the user feels, not the
--   total cursor-open duration.
CREATE TABLE query_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          TEXT    NOT NULL DEFAULT (datetime('now')),
    server_id   INTEGER NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    database    TEXT    NOT NULL,
    sql         TEXT    NOT NULL,
    duration_ms INTEGER NOT NULL,
    ok          INTEGER NOT NULL CHECK (ok IN (0, 1)),
    error       TEXT
);

-- Newest-first scans are the dominant access pattern (history panel).
CREATE INDEX query_history_id_desc ON query_history (id DESC);

-- Optional filter by server in the history panel.
CREATE INDEX query_history_server ON query_history (server_id, id DESC);

-- saved_queries: named SQL snippets, either global or scoped to one server.
--
-- `scope` is enforced at the schema level so application code can trust the
-- invariant.  Per-server rows cascade on connection delete; global rows do
-- not have a server_id and survive any connection delete.
CREATE TABLE saved_queries (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL,
    scope       TEXT    NOT NULL CHECK (scope IN ('global', 'server')),
    server_id   INTEGER REFERENCES connections(id) ON DELETE CASCADE,
    sql         TEXT    NOT NULL,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    CHECK (
        (scope = 'global' AND server_id IS NULL)
        OR
        (scope = 'server' AND server_id IS NOT NULL)
    )
);

-- Lookups by scope + server are the dominant access pattern (Saved panel).
CREATE INDEX saved_queries_scope_server ON saved_queries (scope, server_id, name);
