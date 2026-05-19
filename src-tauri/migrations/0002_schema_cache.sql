CREATE TABLE schema_cache (
    server_id    INTEGER NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    database     TEXT    NOT NULL,
    payload_json TEXT    NOT NULL,
    fetched_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (server_id, database)
);
