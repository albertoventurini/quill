# M5.1 — Migration `0003_history_saved.sql` + `history` / `saved` store modules

## Goal

**Before (post-M4):** The local SQLite store at `<app_data_dir>/quill.sqlite` has two
migrations (`0001_initial.sql` connections, `0002_schema_cache.sql` an unused
table that pre-dates the session-scoped in-memory cache). The `store` module
owns connections CRUD; nothing else writes to SQLite. The query path
(`commands::run_query` → `query::run_query`) hasn't been instrumented for
history — every execution is forgotten as soon as the cursor closes.

**After:** A new migration `0003_history_saved.sql` adds two tables —
`query_history` and `saved_queries` — per `PRD.md` §10 (with one deliberate
deviation, see below). Two new Rust modules — `src-tauri/src/history/mod.rs`
and `src-tauri/src/saved/mod.rs` — expose typed CRUD plus the retention trim.
Both are **headless** in this task: no Tauri commands, no hook into
`run_query`, no UI. The next task (M5.2) wires the command surface and calls
`history::append` from the query path; M5.3/M5.4 add the UI.

This task is **backend-only**. `pnpm check` should be unaffected (no TS edits).
`./test.sh` gains six new unit tests (three per module).

### Deviation from PRD §10: no `row_count` column

`PRD.md` §10 lists `query_history(... row_count, ok, error)`. We deliberately
drop `row_count` from the M5 schema. Rationale, decided in M5 spec discussion:

- For non-streamed results (single chunk), `row_count` equals the first
  chunk's size — fine.
- For streamed results, `row_count` is unknown until the cursor closes;
  capturing it would force an INSERT-then-UPDATE flow with a pending
  `history_id` stashed on every open cursor (four terminal paths to wire,
  orphan-row semantics to document). ~30 LoC of bookkeeping for a column
  that's "nice to have when scanning history" — not load-bearing.
- `duration_ms` alone is meaningful: it's the time-to-first-chunk (what the
  user *feels* — Postgres execution + first chunk back), not "time until
  cursor close" (which is contaminated by user idle time between Load-more
  clicks).
- Adding the column back later is a one-line migration
  (`ALTER TABLE ADD COLUMN row_count INTEGER`) plus a finalize hook. Cheap
  to defer to v1.1.

Final M5 schema: `query_history(id, ts, server_id, database, sql, duration_ms, ok, error)`.
This deviation should be documented inline in the migration file.

## Current state

### `src-tauri/src/store/mod.rs` — the model to follow

Read in full before starting. `store::open` already runs `sqlx::migrate!("./migrations")`
against the pool, so dropping `0003_history_saved.sql` into the migrations
folder is enough to make it run on startup. Migration discovery is purely
file-based; no Rust changes are needed in `store/mod.rs`.

The store module's idioms — `thiserror`-derived `StoreError`, `sqlx::FromRow`
on rows, `query_as` with explicit column lists, `RETURNING` for INSERT, unit
tests against `sqlite::memory:` with `after_connect` enabling foreign keys —
are the canonical pattern. The two new modules in this task follow it
verbatim.

### `src-tauri/src/lib.rs` — wiring

```rust
pub mod commands;
pub mod introspect;
pub mod parse;
pub mod pg;
pub mod query;
pub mod registry;
pub mod slots;
pub mod store;
```

Add `pub mod history;` and `pub mod saved;` here. No `app.manage()` calls
needed — both modules take the `&SqlitePool` already managed by Tauri.

### `src-tauri/src/commands/mod.rs` — untouched in M5.1

Reference only. M5.2 adds `list_history`, `clear_history`, `list_saved`,
`save_query`, `delete_saved`, `rename_saved` here and wires `history::append`
into `run_query`. **None of that happens in M5.1.**

### `src-tauri/migrations/0002_schema_cache.sql` — already shipped (vestigial)

```sql
CREATE TABLE schema_cache (
    server_id    INTEGER NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    database     TEXT    NOT NULL,
    payload_json TEXT    NOT NULL,
    fetched_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (server_id, database)
);
```

The table exists but nothing reads or writes it post-M3 (the schema cache
moved to in-memory `ServerHandle.schema_cache`). Don't drop it here —
migrations are append-only and removing it would orphan installs that ran
this migration earlier. Leave it as a vestigial table; M6 polish may
formally retire it.

## Design choices baked into this spec

- **One migration file, two tables.** Both are M5 deliverables; splitting
  into `0003_query_history.sql` and `0004_saved_queries.sql` buys nothing.
- **`history::append` is fire-and-forget from the caller's perspective.**
  Returns `Result<(), HistoryError>`; the caller (`run_query` in M5.2) logs
  failures but does not propagate them — a SQLite hiccup must not turn a
  successful Postgres query into an error.
- **Retention trim runs inside `append`.** Per `MILESTONES.md` §M5:
  "Retention trim runs synchronously inside `history::append` (cheap,
  single delete by id range). No background job." One `DELETE` per insert,
  bounded by a constant `HISTORY_RETENTION: usize = 1000` defined in
  `history/mod.rs`. The settings UI in M6 will surface this; for M5 it's a
  constant.
- **`scope` is enforced by CHECK constraint, not by application code.**
  `(scope = 'global' AND server_id IS NULL) OR (scope = 'server' AND server_id IS NOT NULL)`
  — SQLite will reject malformed rows at the boundary, so the Rust layer
  doesn't have to. The CHECK costs nothing and documents the invariant in
  the schema itself.
- **Saved-query `name` is **not** UNIQUE.** A global "users" and a
  server-scoped "users" must coexist. Application code rejects exact
  duplicates within the same `(scope, server_id)` slice; the schema doesn't
  enforce it.
- **Foreign key on `query_history.server_id`:** `ON DELETE CASCADE` to
  `connections(id)`. Same for `saved_queries.server_id` (nullable, so
  cascade only fires for server-scoped rows). Matches the
  `schema_cache → connections` pattern from `0002`.
- **`ts` and `created_at` are TEXT (ISO-8601 UTC).** Defaults to
  `datetime('now')` — same idiom as `connections.created_at`. We don't
  need millisecond precision in history; second precision is enough for
  user-facing display.
- **No `updated_at` on `saved_queries`.** Rename mutates `name`; we don't
  track modification time. If a future M wants it, add a column then.
- **Module visibility:** `pub` for functions called from M5.2's commands;
  `#[allow(dead_code)]` on the module while M5.1 lives on the branch
  alone, because nothing inside the binary calls into these modules yet.
  M5.2 removes the allow.

## Deliverables

### 1. `src-tauri/migrations/0003_history_saved.sql` — new file

```sql
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
```

### 2. `src-tauri/src/history/mod.rs` — new module

```rust
#![allow(dead_code)] // M5.2 wires the call sites.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use thiserror::Error;

/// Newest N rows are retained.  Older rows are deleted on insert.
/// M6 surfaces this in a settings panel; for M5 it's a compile-time constant.
pub const HISTORY_RETENTION: usize = 1000;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct HistoryRecord {
    pub id: i64,
    pub ts: String,
    pub server_id: i64,
    pub database: String,
    pub sql: String,
    pub duration_ms: i64,
    /// Stored as INTEGER 0/1 in SQLite; deserialized as bool here.
    pub ok: bool,
    pub error: Option<String>,
}

/// Fields callers supply at insert time.  `id` and `ts` are auto-generated.
#[derive(Debug, Clone)]
pub struct NewHistoryRecord {
    pub server_id: i64,
    pub database: String,
    pub sql: String,
    pub duration_ms: i64,
    pub ok: bool,
    pub error: Option<String>,
}

/// Optional filter for `list`.  Add fields as the UI needs them.
#[derive(Debug, Default, Clone)]
pub struct HistoryFilter {
    pub server_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// Insert a new history row and trim the table to [`HISTORY_RETENTION`].
///
/// The trim runs inside the same call, in a transaction with the insert, so
/// concurrent inserts can't race past the retention bound by more than one
/// row each.  Trim cost is O(1) — a single DELETE bounded by a sub-SELECT.
///
/// **Errors:** propagated as [`HistoryError::Sqlx`].  Callers in the
/// `commands` layer are expected to *log and swallow* — a SQLite hiccup
/// must not surface as a Postgres failure to the user.
pub async fn append(pool: &SqlitePool, r: NewHistoryRecord) -> Result<(), HistoryError> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO query_history (server_id, database, sql, duration_ms, ok, error) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(r.server_id)
    .bind(&r.database)
    .bind(&r.sql)
    .bind(r.duration_ms)
    .bind(r.ok as i32)
    .bind(&r.error)
    .execute(&mut *tx)
    .await?;

    // Trim: delete rows whose id is *not* among the newest HISTORY_RETENTION
    // (by id, since id is monotonically increasing under AUTOINCREMENT).
    sqlx::query(
        "DELETE FROM query_history \
         WHERE id NOT IN ( \
             SELECT id FROM query_history ORDER BY id DESC LIMIT ? \
         )",
    )
    .bind(HISTORY_RETENTION as i64)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// List history rows, newest first.
///
/// `limit` caps the result; pass `HISTORY_RETENTION` to fetch everything.
/// `filter.server_id` narrows to one server when set.
pub async fn list(
    pool: &SqlitePool,
    limit: i64,
    filter: HistoryFilter,
) -> Result<Vec<HistoryRecord>, HistoryError> {
    let rows = if let Some(sid) = filter.server_id {
        sqlx::query_as::<_, HistoryRecord>(
            "SELECT id, ts, server_id, database, sql, duration_ms, \
                    ok AS \"ok!: bool\", error \
             FROM query_history \
             WHERE server_id = ? \
             ORDER BY id DESC \
             LIMIT ?",
        )
        .bind(sid)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, HistoryRecord>(
            "SELECT id, ts, server_id, database, sql, duration_ms, \
                    ok AS \"ok!: bool\", error \
             FROM query_history \
             ORDER BY id DESC \
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(pool)
        .await?
    };
    Ok(rows)
}

/// Delete every row in `query_history`.  Used by the "Clear history" action
/// in M5.4's UI.
pub async fn clear(pool: &SqlitePool) -> Result<(), HistoryError> {
    sqlx::query("DELETE FROM query_history").execute(pool).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    /// Reuse the store test-pool idiom: in-memory SQLite with migrations
    /// applied and foreign keys enabled.
    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("PRAGMA foreign_keys = ON")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// Insert a saved connection so history rows have a valid FK target.
    async fn seed_connection(pool: &SqlitePool, name: &str) -> i64 {
        let conn = store::insert(
            pool,
            store::NewConnection {
                name: name.into(),
                host: "localhost".into(),
                port: 5432,
                default_db: "postgres".into(),
                username: "alice".into(),
                ssl_mode: "prefer".into(),
                slot_budget: 2,
                password_ref: None,
            },
        )
        .await
        .unwrap();
        conn.id
    }

    fn sample(sid: i64, sql: &str, ok: bool) -> NewHistoryRecord {
        NewHistoryRecord {
            server_id: sid,
            database: "postgres".into(),
            sql: sql.into(),
            duration_ms: 42,
            ok,
            error: if ok { None } else { Some("boom".into()) },
        }
    }

    #[tokio::test]
    async fn append_then_list_returns_row_newest_first() {
        let pool = test_pool().await;
        let sid = seed_connection(&pool, "srv").await;

        append(&pool, sample(sid, "SELECT 1", true)).await.unwrap();
        append(&pool, sample(sid, "SELECT 2", true)).await.unwrap();
        append(&pool, sample(sid, "SELECT BROKEN", false))
            .await
            .unwrap();

        let rows = list(&pool, 10, HistoryFilter::default()).await.unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].sql, "SELECT BROKEN");
        assert!(!rows[0].ok);
        assert_eq!(rows[0].error.as_deref(), Some("boom"));
        assert_eq!(rows[2].sql, "SELECT 1");
        assert!(rows[2].ok);
    }

    #[tokio::test]
    async fn list_filters_by_server_id() {
        let pool = test_pool().await;
        let a = seed_connection(&pool, "a").await;
        let b = seed_connection(&pool, "b").await;

        append(&pool, sample(a, "SELECT a1", true)).await.unwrap();
        append(&pool, sample(b, "SELECT b1", true)).await.unwrap();
        append(&pool, sample(a, "SELECT a2", true)).await.unwrap();

        let a_only = list(
            &pool,
            10,
            HistoryFilter {
                server_id: Some(a),
            },
        )
        .await
        .unwrap();
        assert_eq!(a_only.len(), 2);
        assert!(a_only.iter().all(|r| r.server_id == a));
    }

    #[tokio::test]
    async fn append_enforces_retention_cap() {
        // Override the constant for the duration of the test would be ideal,
        // but `const` can't be patched.  Instead, insert HISTORY_RETENTION + 5
        // rows and assert the table is capped.  Slow-ish (1005 inserts) but
        // bounded; runs in ~200ms on a typical dev box.
        let pool = test_pool().await;
        let sid = seed_connection(&pool, "srv").await;

        for i in 0..(HISTORY_RETENTION + 5) {
            append(&pool, sample(sid, &format!("SELECT {i}"), true))
                .await
                .unwrap();
        }

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM query_history")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, HISTORY_RETENTION as i64);

        // The retained rows are the newest ones — the first 5 SELECTs are gone.
        let rows = list(&pool, HISTORY_RETENTION as i64, HistoryFilter::default())
            .await
            .unwrap();
        assert!(rows.iter().all(|r| {
            let n: usize = r.sql.trim_start_matches("SELECT ").parse().unwrap();
            n >= 5
        }));
    }

    #[tokio::test]
    async fn clear_empties_the_table() {
        let pool = test_pool().await;
        let sid = seed_connection(&pool, "srv").await;
        append(&pool, sample(sid, "SELECT 1", true)).await.unwrap();
        append(&pool, sample(sid, "SELECT 2", true)).await.unwrap();

        clear(&pool).await.unwrap();

        let rows = list(&pool, 10, HistoryFilter::default()).await.unwrap();
        assert!(rows.is_empty());
    }
}
```

### 3. `src-tauri/src/saved/mod.rs` — new module

```rust
#![allow(dead_code)] // M5.2 wires the call sites.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum SavedError {
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("Saved query not found: id {0}")]
    NotFound(i64),

    #[error("A saved query named {0:?} already exists in this scope")]
    DuplicateName(String),
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Either `'global'` (visible everywhere) or `'server'` (visible only when
/// the matching server is selected).  The CHECK constraint in the migration
/// enforces the pairing with `server_id` at the schema level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedScope {
    Global,
    Server,
}

impl SavedScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SavedQuery {
    pub id: i64,
    pub name: String,
    /// Stored as TEXT in SQLite ('global' | 'server'); deserialized into the
    /// enum via the `scope_str` field below.  See `from_row_with_scope`.
    #[sqlx(rename = "scope")]
    pub scope_str: String,
    pub server_id: Option<i64>,
    pub sql: String,
    pub created_at: String,
}

impl SavedQuery {
    /// Convenience accessor — parses the stored scope string.  Returns
    /// `Global` for unrecognised values (defensive; the CHECK constraint
    /// prevents this in practice).
    pub fn scope(&self) -> SavedScope {
        match self.scope_str.as_str() {
            "server" => SavedScope::Server,
            _ => SavedScope::Global,
        }
    }
}

/// Fields callers supply when creating a saved query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSavedQuery {
    pub name: String,
    pub scope: SavedScope,
    /// Required when `scope = Server`, must be `None` when `scope = Global`.
    /// `create` validates this pairing before touching the database.
    pub server_id: Option<i64>,
    pub sql: String,
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// List saved queries.  Pass `Some(sid)` to return *only* server-scoped rows
/// for that server (global rows are returned alongside).  Pass `None` to
/// return *only* global rows.
///
/// Result is ordered by scope (global first, then server) then by name.
pub async fn list(
    pool: &SqlitePool,
    server_id: Option<i64>,
) -> Result<Vec<SavedQuery>, SavedError> {
    let rows = if let Some(sid) = server_id {
        sqlx::query_as::<_, SavedQuery>(
            "SELECT id, name, scope, server_id, sql, created_at \
             FROM saved_queries \
             WHERE scope = 'global' \
                OR (scope = 'server' AND server_id = ?) \
             ORDER BY scope DESC, name", // 'server' > 'global' alphabetically; DESC puts global first
        )
        .bind(sid)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, SavedQuery>(
            "SELECT id, name, scope, server_id, sql, created_at \
             FROM saved_queries \
             WHERE scope = 'global' \
             ORDER BY name",
        )
        .fetch_all(pool)
        .await?
    };
    Ok(rows)
}

/// Create a saved query.  Returns the new row (with auto-generated id and
/// created_at).
///
/// Rejects `(scope=Global, server_id=Some)` and `(scope=Server, server_id=None)`
/// before hitting the database.  Rejects duplicate names within the same
/// `(scope, server_id)` slice with `SavedError::DuplicateName`.
pub async fn create(
    pool: &SqlitePool,
    new: NewSavedQuery,
) -> Result<SavedQuery, SavedError> {
    // Validate the scope/server_id pairing.  The CHECK constraint would
    // reject it too, but a dedicated error is friendlier to the UI.
    match (new.scope, new.server_id) {
        (SavedScope::Global, Some(_)) | (SavedScope::Server, None) => {
            return Err(SavedError::Sqlx(sqlx::Error::Protocol(
                "scope/server_id mismatch".into(),
            )));
        }
        _ => {}
    }

    // Check duplicate name in the same slice.
    let dup: Option<(i64,)> = if let Some(sid) = new.server_id {
        sqlx::query_as(
            "SELECT id FROM saved_queries \
             WHERE scope = 'server' AND server_id = ? AND name = ?",
        )
        .bind(sid)
        .bind(&new.name)
        .fetch_optional(pool)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id FROM saved_queries WHERE scope = 'global' AND name = ?",
        )
        .bind(&new.name)
        .fetch_optional(pool)
        .await?
    };
    if dup.is_some() {
        return Err(SavedError::DuplicateName(new.name));
    }

    let row: SavedQuery = sqlx::query_as(
        "INSERT INTO saved_queries (name, scope, server_id, sql) \
         VALUES (?, ?, ?, ?) \
         RETURNING id, name, scope, server_id, sql, created_at",
    )
    .bind(&new.name)
    .bind(new.scope.as_str())
    .bind(new.server_id)
    .bind(&new.sql)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Delete a saved query by id.  Returns `NotFound` if no row was deleted.
pub async fn delete(pool: &SqlitePool, id: i64) -> Result<(), SavedError> {
    let n = sqlx::query("DELETE FROM saved_queries WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(SavedError::NotFound(id));
    }
    Ok(())
}

/// Rename a saved query.  Rejects duplicates within the same `(scope, server_id)`
/// slice with `DuplicateName`.  Returns `NotFound` if the id doesn't exist.
pub async fn rename(
    pool: &SqlitePool,
    id: i64,
    new_name: &str,
) -> Result<SavedQuery, SavedError> {
    // Fetch current row for the duplicate check.
    let current: SavedQuery =
        sqlx::query_as("SELECT id, name, scope, server_id, sql, created_at FROM saved_queries WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or(SavedError::NotFound(id))?;

    let dup: Option<(i64,)> = if let Some(sid) = current.server_id {
        sqlx::query_as(
            "SELECT id FROM saved_queries \
             WHERE scope = 'server' AND server_id = ? AND name = ? AND id <> ?",
        )
        .bind(sid)
        .bind(new_name)
        .bind(id)
        .fetch_optional(pool)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id FROM saved_queries \
             WHERE scope = 'global' AND name = ? AND id <> ?",
        )
        .bind(new_name)
        .bind(id)
        .fetch_optional(pool)
        .await?
    };
    if dup.is_some() {
        return Err(SavedError::DuplicateName(new_name.to_string()));
    }

    let row: SavedQuery = sqlx::query_as(
        "UPDATE saved_queries SET name = ? WHERE id = ? \
         RETURNING id, name, scope, server_id, sql, created_at",
    )
    .bind(new_name)
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("PRAGMA foreign_keys = ON")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn seed_connection(pool: &SqlitePool, name: &str) -> i64 {
        store::insert(
            pool,
            store::NewConnection {
                name: name.into(),
                host: "localhost".into(),
                port: 5432,
                default_db: "postgres".into(),
                username: "alice".into(),
                ssl_mode: "prefer".into(),
                slot_budget: 2,
                password_ref: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn create_global_and_server_then_list_each() {
        let pool = test_pool().await;
        let a = seed_connection(&pool, "a").await;
        let b = seed_connection(&pool, "b").await;

        create(
            &pool,
            NewSavedQuery {
                name: "users".into(),
                scope: SavedScope::Global,
                server_id: None,
                sql: "SELECT * FROM users".into(),
            },
        )
        .await
        .unwrap();
        create(
            &pool,
            NewSavedQuery {
                name: "a-only".into(),
                scope: SavedScope::Server,
                server_id: Some(a),
                sql: "SELECT 1".into(),
            },
        )
        .await
        .unwrap();
        create(
            &pool,
            NewSavedQuery {
                name: "b-only".into(),
                scope: SavedScope::Server,
                server_id: Some(b),
                sql: "SELECT 2".into(),
            },
        )
        .await
        .unwrap();

        let only_global = list(&pool, None).await.unwrap();
        assert_eq!(only_global.len(), 1);
        assert_eq!(only_global[0].name, "users");

        let for_a = list(&pool, Some(a)).await.unwrap();
        // 'global' DESC < 'server' lexically, so global comes first in our ORDER BY scope DESC.
        assert_eq!(for_a.len(), 2);
        assert!(for_a.iter().any(|r| r.name == "users"));
        assert!(for_a.iter().any(|r| r.name == "a-only"));
        assert!(for_a.iter().all(|r| r.name != "b-only"));
    }

    #[tokio::test]
    async fn duplicate_name_in_same_scope_is_rejected() {
        let pool = test_pool().await;
        let a = seed_connection(&pool, "a").await;

        create(
            &pool,
            NewSavedQuery {
                name: "dup".into(),
                scope: SavedScope::Server,
                server_id: Some(a),
                sql: "SELECT 1".into(),
            },
        )
        .await
        .unwrap();

        let err = create(
            &pool,
            NewSavedQuery {
                name: "dup".into(),
                scope: SavedScope::Server,
                server_id: Some(a),
                sql: "SELECT 2".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SavedError::DuplicateName(_)));

        // Same name on a *different* server is fine.
        let b = seed_connection(&pool, "b").await;
        create(
            &pool,
            NewSavedQuery {
                name: "dup".into(),
                scope: SavedScope::Server,
                server_id: Some(b),
                sql: "SELECT 3".into(),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn rename_then_delete_round_trip() {
        let pool = test_pool().await;
        let row = create(
            &pool,
            NewSavedQuery {
                name: "old".into(),
                scope: SavedScope::Global,
                server_id: None,
                sql: "SELECT 1".into(),
            },
        )
        .await
        .unwrap();

        let renamed = rename(&pool, row.id, "new").await.unwrap();
        assert_eq!(renamed.name, "new");

        delete(&pool, row.id).await.unwrap();
        let err = delete(&pool, row.id).await.unwrap_err();
        assert!(matches!(err, SavedError::NotFound(_)));
    }

    #[tokio::test]
    async fn scope_check_constraint_blocks_mismatch_at_sql_level() {
        // We don't expose a way to construct a mismatched row through the
        // public API (`create` rejects it), so prove the schema-level check
        // is also there.
        let pool = test_pool().await;
        let err = sqlx::query(
            "INSERT INTO saved_queries (name, scope, server_id, sql) \
             VALUES ('bad', 'global', 1, 'SELECT 1')",
        )
        .execute(&pool)
        .await
        .unwrap_err();
        // SQLite reports this as a CHECK constraint failure.
        assert!(err.to_string().to_lowercase().contains("check"));
    }
}
```

### 4. `src-tauri/src/lib.rs` — register the modules

Insert after `pub mod commands;`:

```rust
pub mod commands;
pub mod history;       // <-- new
pub mod introspect;
pub mod parse;
pub mod pg;
pub mod query;
pub mod registry;
pub mod saved;         // <-- new
pub mod slots;
pub mod store;
```

No other `lib.rs` changes — no `app.manage()` calls, no `invoke_handler!`
additions. M5.2 wires the commands.

## Implementation order

1. **`src-tauri/migrations/0003_history_saved.sql`** — write the migration first.
2. **`src-tauri/src/history/mod.rs`** — new file. Compiles in isolation (nothing
   imports it yet).
3. **`src-tauri/src/saved/mod.rs`** — new file. Same.
4. **`src-tauri/src/lib.rs`** — add `pub mod history; pub mod saved;`.
5. `( cd src-tauri && cargo build )` — should compile clean with two
   `#[allow(dead_code)]` modules.
6. `( cd src-tauri && cargo test history saved )` — 4 history tests + 4
   saved tests = 8 new tests, all passing.
7. `./test.sh` — full suite passes (existing tests unaffected).
8. Smoke: `./run.sh` boots; nothing user-visible changes, but the migration
   ran. Check with `sqlite3 ~/.local/share/com.alberto.quill/quill.sqlite
   ".tables"` — should list `query_history` and `saved_queries`.

## Known gotchas

- **`sqlx::FromRow` and `bool` for the `ok` column.** SQLite stores booleans
  as INTEGER 0/1; sqlx maps `bool` to/from that automatically. The
  `query_as::<_, HistoryRecord>` calls use `ok AS \"ok!: bool\"` to assert
  the type in SQL, which keeps sqlx's runtime type check happy. Without the
  cast, sqlx may decode the column as `i64` and refuse to coerce.
- **Foreign keys are per-connection in SQLite.** `store::open` already
  enables them via `after_connect`. The new tables' FKs will fire only
  through that pool; ad-hoc connections (e.g. opening the sqlite file with
  the CLI without `PRAGMA foreign_keys = ON`) will not enforce them. Not a
  bug — just don't rely on it from raw SQL outside the app.
- **`ORDER BY scope DESC` puts 'global' before 'server'.** Reason: ASCII
  ordering — `'g'` < `'s'` — and `DESC` flips it. Counter-intuitive but the
  test asserts the order, so any drift surfaces immediately. The alternative
  (`CASE WHEN scope = 'global' THEN 0 ELSE 1 END`) is wordier and equivalent.
- **`AUTOINCREMENT` on `query_history.id`.** Without it, SQLite may reuse
  ids after deletes; the index `query_history_id_desc` relies on
  monotonicity for the retention trim to keep the *newest* rows. Same idiom
  as `connections.id`.
- **Retention trim runs in the same transaction as the insert.** This
  serializes the trim against concurrent inserts on the same pool. Pool
  size is 1 (per `store::open`), so there's effectively no concurrency,
  but the transaction makes the semantics correct under any size.
- **`HISTORY_RETENTION` is 1000 by design.** Per `MILESTONES.md` §M5: the
  M5 ship value lives in a constant; M6 settings panel makes it editable.
- **`sqlx::Error::Protocol` for validation errors in `create`.** Mildly
  abusive — `Protocol` is technically for wire-level errors — but it
  preserves the `sqlx::Error` discriminant without inventing a new variant.
  A cleaner alternative is a dedicated `SavedError::InvalidScope` variant;
  do that if a future task surfaces these mismatches in the UI.
- **`saved_queries.name` is NOT UNIQUE in the schema** — duplicate
  prevention lives in Rust (`create`/`rename`) because the legitimate
  duplicate case ("users" global + "users" on server A) is allowed.
  Schema-level UNIQUE would have to be composite (`scope, server_id, name`)
  and SQLite UNIQUE constraints treat NULL as distinct, which breaks the
  global-row case. Easier to enforce in code.
- **`#[allow(dead_code)]` at the module top** keeps `cargo clippy` quiet
  while M5.1 lives alone. M5.2 removes it once the call sites are wired.
- **No new dependencies.** Everything used is already in `Cargo.toml`
  (`sqlx`, `serde`, `thiserror`, `tokio`).
- **Tests with `seed_connection` are slow because of `sqlx::migrate!`** —
  each test rebuilds the pool. ~50ms per test × 8 tests = ~400ms. Acceptable;
  matches the store test pattern.
- **`HistoryFilter::default()`** is `{ server_id: None }`. If you add a
  `since: Option<DateTime>` field later, default to `None` so existing
  callers stay unaffected.
- **`scope_str` on `SavedQuery`** is the storage shape; `scope()` is the
  accessor. The frontend will receive `scope_str` (renamed to `scope` in
  the JSON output thanks to `#[sqlx(rename = "scope")]` not affecting
  serde — see next gotcha).
- **`#[sqlx(rename = "scope")]` does not change serde's output name.**
  When the frontend mirror type lands in M5.2, it sees `scope_str` as the
  JSON key. If you want a clean `scope` key in the JSON, add
  `#[serde(rename = "scope")]` alongside it. (Defer to M5.2 — that task
  ships the JSON shape and adds the TS mirror.)

## Tests

Run via `./test.sh`. Eight new unit tests:

**`history` (4):**
- `append_then_list_returns_row_newest_first`
- `list_filters_by_server_id`
- `append_enforces_retention_cap` — inserts `HISTORY_RETENTION + 5` rows,
  asserts the table is capped and the oldest are gone.
- `clear_empties_the_table`

**`saved` (4):**
- `create_global_and_server_then_list_each`
- `duplicate_name_in_same_scope_is_rejected` — also verifies same name on
  a *different* server is fine.
- `rename_then_delete_round_trip`
- `scope_check_constraint_blocks_mismatch_at_sql_level` — proves the
  schema-level CHECK is also enforced (not just the Rust validation).

No integration tests in M5.1 — no Postgres needed. No frontend changes.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds — 8 new unit tests pass; existing suite unaffected.
- [ ] `pnpm check` succeeds (no TS changes; sanity).
- [ ] `git status` shows three new files: `migrations/0003_history_saved.sql`,
      `src-tauri/src/history/mod.rs`, `src-tauri/src/saved/mod.rs`.
- [ ] `grep -n "pub mod history;\|pub mod saved;" src-tauri/src/lib.rs` shows
      both.
- [ ] `grep -F "HISTORY_RETENTION: usize = 1000" src-tauri/src/history/mod.rs`
      matches.
- [ ] `grep -F "row_count" src-tauri/migrations/0003_history_saved.sql` returns
      **zero** matches — the deviation from PRD §10 is intentional and the
      column must not be present.
- [ ] After `./run.sh`, `sqlite3 ~/.local/share/com.alberto.quill/quill.sqlite
      ".tables"` lists `query_history` and `saved_queries`.
- [ ] No new dependencies in `Cargo.toml` or `package.json`.

## Out of scope

- Tauri commands for the new modules — **M5.2**.
- Calling `history::append` from `run_query` — **M5.2**.
- Frontend bindings, tabs, side panel, CSV — **M5.3 / M5.4 / M5.5**.
- Per-server retention overrides; surfacing the retention constant in
  settings — **M6**.
- Tags / folders on saved queries — v1.1.
- A `last_used_at` column on saved queries — v1.1.
- Restoring `row_count` to history — deferrable to v1.1 via additive
  migration; explicitly out of scope here.
- Removing the vestigial `schema_cache` table (`0002_schema_cache.sql`) —
  **M6** polish; migrations are append-only and the table is harmless.
