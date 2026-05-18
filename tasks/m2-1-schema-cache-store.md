# M2.1 — `schema_cache` migration + store helpers

## Goal

**Before:** The local SQLite store has exactly one table, `connections`, populated by M1.2. There is no place to persist introspected Postgres schema data; every tree expansion in a future M2.4 UI would have to re-query the server. `src-tauri/src/store/mod.rs` exposes `open`, `list`, `get`, `insert`, `delete`, `Connection`, `NewConnection`, and `StoreError` — nothing else.

**After:** A second migration `0002_schema_cache.sql` creates the `schema_cache(server_id, database, payload_json, fetched_at)` table per `PRD.md` §10, keyed on `(server_id, database)`. Foreign keys are enabled per-connection so deleting a row from `connections` cascades to its cache rows. `store/mod.rs` gains four new free functions — `get_schema_cache`, `set_schema_cache`, `delete_schema_cache`, `delete_schema_cache_for_server` — and a `SchemaCacheRow` value type that wraps the JSON blob along with its `fetched_at` timestamp. This task is **headless**: no Tauri commands, no Postgres calls, no UI. Output is one new SQL file plus a handful of additions to the existing store module, all covered by unit tests against an in-memory SQLite database.

## Current state

Every file below already exists and is reproduced in full (or in relevant excerpts). Read them before writing anything.

### `src-tauri/migrations/0001_initial.sql`

```sql
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
```

This file is **not** modified. The new table lives in a separate migration so the migrator records them as distinct steps and existing deployments can apply only the new one on next boot.

### `src-tauri/src/store/mod.rs` — `open` and module top

The `open` function currently creates the directory, opens the pool with `max_connections(1)`, and runs migrations. It does **not** enable SQLite foreign keys. That must change in this task — `PRAGMA foreign_keys` defaults to `OFF` in SQLite and is per-connection, so it must be set on every connection via sqlx's `after_connect` hook.

Current shape (excerpt; the rest of the module stays):

```rust
pub async fn open(app: &tauri::AppHandle) -> Result<SqlitePool, StoreError> {
    use tauri::Manager;

    let app_dir = app.path().app_data_dir().map_err(std::io::Error::other)?;
    std::fs::create_dir_all(&app_dir)?;

    let db_path = app_dir.join("quill.sqlite");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}
```

### `src-tauri/src/store/mod.rs` — `StoreError`

Unchanged by this task; new functions reuse the existing variants:

```rust
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("Migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
```

### `src-tauri/src/store/mod.rs` — test helper

The existing in-memory test helper applies migrations and is the pattern the new tests must follow:

```rust
async fn test_pool() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}
```

The new tests must reuse it, **but** the FK-cascade test additionally needs `PRAGMA foreign_keys = ON` to be set on the test pool. The fix is to switch `test_pool()` to the same builder shape as production `open` — see deliverable 2 for the exact code.

### `src-tauri/src/lib.rs`

Already declares `pub mod store;` and calls `store::open` in `setup`. **Not modified** by this task — the store API additions are picked up automatically by future tasks that import them.

## Deliverables

### 1. `src-tauri/migrations/0002_schema_cache.sql` — new file

```sql
CREATE TABLE schema_cache (
    server_id    INTEGER NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    database     TEXT    NOT NULL,
    payload_json TEXT    NOT NULL,
    fetched_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (server_id, database)
);
```

Notes on the choices:

- **One row per `(server_id, database)`**, not per schema. The introspection payload from M2.2 holds every schema in that DB; smaller cache rows would multiply round-trips for no win. `MILESTONES.md` §M2 spells this out.
- **`payload_json` is `TEXT`, not `BLOB`.** SQLite stores both as bytes; `TEXT` is easier to inspect with `sqlite3` at debug time, and `serde_json::to_string` already produces UTF-8.
- **`ON DELETE CASCADE`** on `server_id`. Deleting a saved connection must drop its cached schemas — otherwise stale rows accumulate for ids that will never reappear. Requires the per-connection PRAGMA in deliverable 2; without that, the FK is parsed but ignored at runtime.
- **No `UPDATE` trigger on `fetched_at`.** Writes go through the `set_schema_cache` helper, which sets the timestamp explicitly via `datetime('now')` in the `INSERT OR REPLACE`. Triggers would diverge from the default value if the helper ever wanted to backdate (it won't in v1, but the trigger forecloses the option).
- **No index on `(server_id, database)` separately** — that pair *is* the primary key, which SQLite already indexes.

### 2. `src-tauri/src/store/mod.rs` — turn on foreign keys + add cache helpers

Three changes inside the existing file. Do not move surrounding code; insert the additions in the locations described.

#### 2a. Enable `PRAGMA foreign_keys = ON` on every pool connection

Replace the body of `open` with the version below. The only functional change is the `after_connect` hook — directory creation, URL building, and `migrate!` are unchanged.

```rust
pub async fn open(app: &tauri::AppHandle) -> Result<SqlitePool, StoreError> {
    use tauri::Manager;

    let app_dir = app.path().app_data_dir().map_err(std::io::Error::other)?;
    std::fs::create_dir_all(&app_dir)?;

    let db_path = app_dir.join("quill.sqlite");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                // Foreign keys are per-connection in SQLite and default OFF.
                // Without this, the schema_cache → connections cascade is
                // parsed but never fires at runtime.
                sqlx::query("PRAGMA foreign_keys = ON")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}
```

#### 2b. Add the cache value type

Insert below the existing `NewConnection` struct, in the **Data types** section:

```rust
/// One row of the local `schema_cache` table.
///
/// `payload_json` is the raw JSON string written by M2.2's introspection;
/// callers parse it into the typed `introspect::SchemaPayload` themselves.
/// Keeping the wire shape opaque here lets the store module stay independent
/// of the introspection module.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SchemaCacheRow {
    pub server_id: i64,
    pub database: String,
    pub payload_json: String,
    pub fetched_at: String,
}
```

#### 2c. Add the four cache helpers

Insert at the end of the **CRUD** section, before `#[cfg(test)] mod tests`:

```rust
// ---------------------------------------------------------------------------
// Schema cache CRUD
// ---------------------------------------------------------------------------

/// Fetch the cached schema payload for `(server_id, database)`.
pub async fn get_schema_cache(
    pool: &SqlitePool,
    server_id: i64,
    database: &str,
) -> Result<Option<SchemaCacheRow>, StoreError> {
    Ok(sqlx::query_as::<_, SchemaCacheRow>(
        "SELECT server_id, database, payload_json, fetched_at \
         FROM schema_cache \
         WHERE server_id = ? AND database = ?",
    )
    .bind(server_id)
    .bind(database)
    .fetch_optional(pool)
    .await?)
}

/// Insert-or-replace the cache entry for `(server_id, database)`.
///
/// `fetched_at` is always set to the current UTC time — callers cannot
/// backdate (no use case in v1).
pub async fn set_schema_cache(
    pool: &SqlitePool,
    server_id: i64,
    database: &str,
    payload_json: &str,
) -> Result<SchemaCacheRow, StoreError> {
    Ok(sqlx::query_as::<_, SchemaCacheRow>(
        "INSERT INTO schema_cache (server_id, database, payload_json, fetched_at) \
         VALUES (?, ?, ?, datetime('now')) \
         ON CONFLICT(server_id, database) DO UPDATE SET \
             payload_json = excluded.payload_json, \
             fetched_at   = excluded.fetched_at \
         RETURNING server_id, database, payload_json, fetched_at",
    )
    .bind(server_id)
    .bind(database)
    .bind(payload_json)
    .fetch_one(pool)
    .await?)
}

/// Delete the cache entry for `(server_id, database)`. Ok if no row existed.
pub async fn delete_schema_cache(
    pool: &SqlitePool,
    server_id: i64,
    database: &str,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM schema_cache WHERE server_id = ? AND database = ?")
        .bind(server_id)
        .bind(database)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete every cache row for `server_id` (useful when the user disconnects
/// and wants a fresh introspection on next connect — currently unused by M2,
/// but the symmetric counterpart of `set_schema_cache` is worth shipping).
pub async fn delete_schema_cache_for_server(
    pool: &SqlitePool,
    server_id: i64,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM schema_cache WHERE server_id = ?")
        .bind(server_id)
        .execute(pool)
        .await?;
    Ok(())
}
```

#### 2d. Replace the `test_pool` helper

Switch `test_pool()` (inside `#[cfg(test)] mod tests`) to apply the same `after_connect` hook the production builder does. Without this, the FK cascade test in step 3 will silently pass because the constraint isn't enforced:

```rust
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
```

### 3. New unit tests (inside the existing `#[cfg(test)] mod tests`)

Add the tests below at the end of the existing `tests` module. Reuse `test_pool()` and `sample_new()`.

```rust
fn sample_payload() -> &'static str {
    // Shape-only fixture — M2.2 will define the canonical SchemaPayload,
    // but for storage tests any UTF-8 string works.
    r#"{"v":1,"schemas":[]}"#
}

#[tokio::test]
async fn schema_cache_set_returns_row_with_fetched_at() {
    let pool = test_pool().await;
    let server = insert(&pool, sample_new("srv")).await.unwrap();

    let row = set_schema_cache(&pool, server.id, "postgres", sample_payload())
        .await
        .unwrap();

    assert_eq!(row.server_id, server.id);
    assert_eq!(row.database, "postgres");
    assert_eq!(row.payload_json, sample_payload());
    assert!(!row.fetched_at.is_empty(), "fetched_at must be set");
}

#[tokio::test]
async fn schema_cache_get_returns_set_row() {
    let pool = test_pool().await;
    let server = insert(&pool, sample_new("srv")).await.unwrap();
    set_schema_cache(&pool, server.id, "db1", sample_payload())
        .await
        .unwrap();

    let got = get_schema_cache(&pool, server.id, "db1").await.unwrap();
    assert!(got.is_some());
    let got = got.unwrap();
    assert_eq!(got.payload_json, sample_payload());

    let missing = get_schema_cache(&pool, server.id, "db-nope")
        .await
        .unwrap();
    assert!(missing.is_none());
}

#[tokio::test]
async fn schema_cache_set_is_upsert() {
    let pool = test_pool().await;
    let server = insert(&pool, sample_new("srv")).await.unwrap();

    let first = set_schema_cache(&pool, server.id, "db1", r#"{"v":1,"schemas":[]}"#)
        .await
        .unwrap();
    let second = set_schema_cache(&pool, server.id, "db1", r#"{"v":1,"schemas":[{"name":"public","relations":[],"functions":[]}]}"#)
        .await
        .unwrap();

    assert_eq!(first.server_id, second.server_id);
    assert_eq!(first.database, second.database);
    assert_ne!(first.payload_json, second.payload_json);

    let got = get_schema_cache(&pool, server.id, "db1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.payload_json, second.payload_json);
}

#[tokio::test]
async fn schema_cache_delete_removes_single_db() {
    let pool = test_pool().await;
    let server = insert(&pool, sample_new("srv")).await.unwrap();
    set_schema_cache(&pool, server.id, "db1", sample_payload())
        .await
        .unwrap();
    set_schema_cache(&pool, server.id, "db2", sample_payload())
        .await
        .unwrap();

    delete_schema_cache(&pool, server.id, "db1").await.unwrap();

    assert!(get_schema_cache(&pool, server.id, "db1")
        .await
        .unwrap()
        .is_none());
    assert!(get_schema_cache(&pool, server.id, "db2")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn schema_cache_delete_for_server_clears_all_dbs() {
    let pool = test_pool().await;
    let server = insert(&pool, sample_new("srv")).await.unwrap();
    set_schema_cache(&pool, server.id, "db1", sample_payload())
        .await
        .unwrap();
    set_schema_cache(&pool, server.id, "db2", sample_payload())
        .await
        .unwrap();

    delete_schema_cache_for_server(&pool, server.id)
        .await
        .unwrap();

    assert!(get_schema_cache(&pool, server.id, "db1")
        .await
        .unwrap()
        .is_none());
    assert!(get_schema_cache(&pool, server.id, "db2")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn deleting_a_connection_cascades_cache_rows() {
    // This is the only test that exercises ON DELETE CASCADE.  It will pass
    // silently if PRAGMA foreign_keys is OFF — see the test_pool helper.
    let pool = test_pool().await;
    let a = insert(&pool, sample_new("a")).await.unwrap();
    let b = insert(&pool, sample_new("b")).await.unwrap();
    set_schema_cache(&pool, a.id, "db", sample_payload())
        .await
        .unwrap();
    set_schema_cache(&pool, b.id, "db", sample_payload())
        .await
        .unwrap();

    delete(&pool, a.id).await.unwrap();

    assert!(get_schema_cache(&pool, a.id, "db")
        .await
        .unwrap()
        .is_none());
    assert!(get_schema_cache(&pool, b.id, "db")
        .await
        .unwrap()
        .is_some());
}
```

## Implementation order

1. **`src-tauri/migrations/0002_schema_cache.sql`** — write the new migration. Nothing references it yet; `cargo build` still succeeds.
2. **`src-tauri/src/store/mod.rs`** — the four edits in deliverable 2 (a, b, c, d). Run `( cd src-tauri && cargo build )` after — must succeed with no new warnings.
3. **Add the new tests** from deliverable 3 to the existing `#[cfg(test)] mod tests`. Run `./test.sh` — every new test must pass, every existing test must still pass.

No intermediate compile errors if you follow this order. The `Cargo.toml` requires no changes — `sqlx`, `tokio`, `serde`, and `thiserror` are already present from M1.

## Known gotchas

- **SQLite foreign keys are per-connection.** Setting them in one migration step or via a `PRAGMA` in `0002_schema_cache.sql` itself would only affect the one connection that ran the migration. The `after_connect` hook is the only correct place. If you skip it, `deleting_a_connection_cascades_cache_rows` returns rows that should have been deleted — and there's no error, the constraint is silently ignored.
- **`sqlx::migrate!` re-applies only new migrations.** It records each filename in `_sqlx_migrations` and skips already-run ones. Adding `0002_schema_cache.sql` does **not** re-run `0001_initial.sql`. Local development DBs (`~/.local/share/com.alberto.quill/quill.sqlite`) pick up the new table on next boot without manual intervention.
- **`ON CONFLICT(...) DO UPDATE SET ... RETURNING ...`** requires SQLite 3.35+ for `RETURNING` and 3.24+ for `ON CONFLICT`. All modern distros (and the version sqlx-sqlite bundles) satisfy both. The existing `INSERT ... RETURNING` in `insert(...)` already relies on 3.35+, so there's no new floor.
- **`PRAGMA foreign_keys` returns no rows.** `sqlx::query("PRAGMA foreign_keys = ON").execute(conn)` is the correct call — `fetch_one` would expect a row and fail. Already what the code does; don't switch.
- **Don't add `WITHOUT ROWID`.** The cache table is small (~one row per database per server) and benefits nothing from `WITHOUT ROWID`. Stick with the implicit rowid; matches the rest of the schema.
- **`SchemaCacheRow` derives `sqlx::FromRow`** — the field order must match the `SELECT` column order in the helpers. Both list `server_id, database, payload_json, fetched_at`. If you reorder one, reorder the other.
- **The `database` column name shadows the SQL keyword.** SQLite does not reserve `database`, so it's safe unquoted. If a future contributor switches the local store to Postgres (unlikely — not in v1), quoting would matter.
- **No new dependencies needed.** Do not add `chrono`, `time`, or any datetime crate — `fetched_at` is a `String` formatted by SQLite's `datetime('now')`, matching `connections.created_at`.

## Tests

Run via `./test.sh`. The new tests live alongside the existing ones inside `store/mod.rs`. Coverage:

- `schema_cache_set_returns_row_with_fetched_at` — round-trip insert + verify `fetched_at` populated.
- `schema_cache_get_returns_set_row` — `get` returns what `set` wrote; `get` on a missing key returns `None`.
- `schema_cache_set_is_upsert` — second `set` on the same key overwrites the payload (not duplicates).
- `schema_cache_delete_removes_single_db` — only the targeted `(server, db)` is affected.
- `schema_cache_delete_for_server_clears_all_dbs` — server-wide wipe.
- `deleting_a_connection_cascades_cache_rows` — proves the FK cascade is live (would fail silently if PRAGMA were off).

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds; all six new store tests pass and the existing six store tests still pass.
- [ ] `grep -RIn "schema_cache" src-tauri/src` shows the new helpers in `store/mod.rs` and nothing else (no Tauri command, no introspect module — those are M2.2/M2.3).
- [ ] `grep -RIn "foreign_keys" src-tauri/src` shows two matches: the `after_connect` hook in `open` and the matching hook in `test_pool`.
- [ ] `ls src-tauri/migrations/` shows `0001_initial.sql` and `0002_schema_cache.sql` (no other files).
- [ ] Booting the app (`./run.sh`) creates the `schema_cache` table — verify with `sqlite3 ~/.local/share/com.alberto.quill/quill.sqlite ".tables"` showing both `connections` and `schema_cache`.
- [ ] `grep -c "ON DELETE CASCADE" src-tauri/migrations/0002_schema_cache.sql` returns `1`.
- [ ] No Tauri commands added (M2.3 owns the command surface for schema cache).
- [ ] No new dependencies in `Cargo.toml`.

## Out of scope

- Introspection queries against Postgres — **M2.2**.
- Tauri commands that expose the cache or introspection — **M2.3**.
- Any frontend code — **M2.4**.
- A typed `SchemaPayload` struct — that lives in `src-tauri/src/introspect/mod.rs` and is M2.2's deliverable. M2.1 keeps the payload opaque (`String`) at the store boundary.
- A `last_used` column or any "refresh stale entries automatically" logic — Quill's caching is manual-refresh only (`AGENTS.md` principle 1 + 3; `PRD.md` §6).
- Cleanup of `schema_cache` rows when the user changes `slot_budget` or other unrelated `connections` fields — only `DELETE` cascades.
- A migration that backfills v0 → v1 payloads — there are no v0 payloads yet.
