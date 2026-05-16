CREATE TABLE connections (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL UNIQUE,
    host          TEXT    NOT NULL,
    port          INTEGER NOT NULL DEFAULT 5432,
    default_db    TEXT    NOT NULL,
    username      TEXT    NOT NULL,         -- "user" is reserved in SQL
    ssl_mode      TEXT    NOT NULL DEFAULT 'prefer',
    slot_budget   INTEGER NOT NULL DEFAULT 2,
    password_ref  TEXT,                     -- opaque keyring id; NULL in M1
    created_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);
