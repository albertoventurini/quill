#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("Migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Connection {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: i32,
    pub default_db: String,
    pub username: String,
    pub ssl_mode: String,
    pub slot_budget: i32,
    pub password_ref: Option<String>,
    pub created_at: String,
}

/// Everything needed to create a new connection, minus auto-generated fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewConnection {
    pub name: String,
    pub host: String,
    pub port: i32,
    pub default_db: String,
    pub username: String,
    pub ssl_mode: String,
    pub slot_budget: i32,
    pub password_ref: Option<String>,
}

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

// ---------------------------------------------------------------------------
// Open / migrate
// ---------------------------------------------------------------------------

/// Open (or create) the local SQLite store at `<app_data_dir>/quill.sqlite`
/// and run pending migrations.
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

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// List all saved connections, ordered by name.
pub async fn list(pool: &SqlitePool) -> Result<Vec<Connection>, StoreError> {
    Ok(sqlx::query_as::<_, Connection>(
        "SELECT id, name, host, port, default_db, username, ssl_mode, slot_budget, \
                password_ref, created_at \
         FROM connections \
         ORDER BY name",
    )
    .fetch_all(pool)
    .await?)
}

/// Get a single connection by id.
pub async fn get(pool: &SqlitePool, id: i64) -> Result<Option<Connection>, StoreError> {
    Ok(sqlx::query_as::<_, Connection>(
        "SELECT id, name, host, port, default_db, username, ssl_mode, slot_budget, \
                password_ref, created_at \
         FROM connections \
         WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// Insert a new connection and return the full row (with auto-generated id/created_at).
pub async fn insert(pool: &SqlitePool, c: NewConnection) -> Result<Connection, StoreError> {
    // We use RETURNING so the full row is returned in one round-trip.
    // SQLite 3.35+; all modern systems ship a recent enough SQLite.
    Ok(sqlx::query_as::<_, Connection>(
        "INSERT INTO connections (name, host, port, default_db, username, ssl_mode, \
                                  slot_budget, password_ref) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING id, name, host, port, default_db, username, ssl_mode, slot_budget, \
                   password_ref, created_at",
    )
    .bind(&c.name)
    .bind(&c.host)
    .bind(c.port)
    .bind(&c.default_db)
    .bind(&c.username)
    .bind(&c.ssl_mode)
    .bind(c.slot_budget)
    .bind(&c.password_ref)
    .fetch_one(pool)
    .await?)
}

/// Delete a connection by id. Returns `Ok(())` even if no row existed.
pub async fn delete(pool: &SqlitePool, id: i64) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM connections WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    /// Helper: create an in-memory SQLite pool with migrations applied.
    /// Uses max_connections(1) so the in-memory database persists for the
    /// lifetime of the pool (each connection gets its own :memory:).
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

    fn sample_new(name: &str) -> NewConnection {
        NewConnection {
            name: name.to_string(),
            host: "localhost".into(),
            port: 5432,
            default_db: "postgres".into(),
            username: "alice".into(),
            ssl_mode: "prefer".into(),
            slot_budget: 2,
            password_ref: None,
        }
    }

    #[tokio::test]
    async fn test_migration_runs_cleanly() {
        let pool = test_pool().await;

        // Verify the connections table exists.
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND name='connections'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();

        assert!(
            row.is_some(),
            "connections table should exist after migration"
        );
    }

    #[tokio::test]
    async fn test_insert_and_list() {
        let pool = test_pool().await;
        let nc = sample_new("my-server");
        let conn = insert(&pool, nc).await.unwrap();

        assert!(conn.id > 0, "insert should assign a positive id");
        assert_eq!(conn.name, "my-server");
        assert_eq!(conn.host, "localhost");
        assert_eq!(conn.port, 5432);

        let all = list(&pool).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, conn.id);
    }

    #[tokio::test]
    async fn test_get_returns_inserted() {
        let pool = test_pool().await;
        let nc = sample_new("get-me");
        let conn = insert(&pool, nc).await.unwrap();

        let found = get(&pool, conn.id).await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "get-me");
    }

    #[tokio::test]
    async fn test_get_unknown_id_returns_none() {
        let pool = test_pool().await;
        let result = get(&pool, 999).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_delete_removes_row() {
        let pool = test_pool().await;
        let nc = sample_new("to-delete");
        let conn = insert(&pool, nc).await.unwrap();

        delete(&pool, conn.id).await.unwrap();

        let after = get(&pool, conn.id).await.unwrap();
        assert!(after.is_none(), "deleted row should not be found");

        let all = list(&pool).await.unwrap();
        assert!(all.is_empty(), "list should be empty after delete");
    }

    #[tokio::test]
    async fn test_name_unique_constraint() {
        let pool = test_pool().await;
        let nc = sample_new("unique-name");
        insert(&pool, nc).await.unwrap();

        let dup = sample_new("unique-name");
        let err = insert(&pool, dup).await.unwrap_err();

        // The error should originate from sqlx and carry a constraint violation.
        match &err {
            StoreError::Sqlx(sqlx::Error::Database(db_err)) => {
                assert!(
                    db_err.is_unique_violation(),
                    "expected unique-violation on duplicate name"
                );
            }
            other => panic!("expected Database unique-violation, got {other:?}"),
        }
    }

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

        let missing = get_schema_cache(&pool, server.id, "db-nope").await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn schema_cache_set_is_upsert() {
        let pool = test_pool().await;
        let server = insert(&pool, sample_new("srv")).await.unwrap();

        let first = set_schema_cache(&pool, server.id, "db1", r#"{"v":1,"schemas":[]}"#)
            .await
            .unwrap();
        let second = set_schema_cache(
            &pool,
            server.id,
            "db1",
            r#"{"v":1,"schemas":[{"name":"public","relations":[],"functions":[]}]}"#,
        )
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

        assert!(
            get_schema_cache(&pool, server.id, "db1")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            get_schema_cache(&pool, server.id, "db2")
                .await
                .unwrap()
                .is_some()
        );
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

        assert!(
            get_schema_cache(&pool, server.id, "db1")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            get_schema_cache(&pool, server.id, "db2")
                .await
                .unwrap()
                .is_none()
        );
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

        assert!(get_schema_cache(&pool, a.id, "db").await.unwrap().is_none());
        assert!(get_schema_cache(&pool, b.id, "db").await.unwrap().is_some());
    }
}
