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
    pub credential_source: String,
    pub bao_role_path: Option<String>,
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
    pub credential_source: String,
    pub bao_role_path: Option<String>,
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

    #[cfg(unix)]
    restrict_permissions(&app_dir, &db_path);

    Ok(pool)
}

/// Keep the local store private to the current user. The app-data dir holds connections,
/// query history and cached schema (and logs); the OpenBao token itself lives in the OS
/// keyring, not here, so this is defense-in-depth for the rest. The WAL/SHM siblings can hold
/// committed data too, hence the best-effort pass over them.
#[cfg(unix)]
fn restrict_permissions(app_dir: &std::path::Path, db_path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let _ = std::fs::set_permissions(app_dir, std::fs::Permissions::from_mode(0o700));
    for suffix in ["", "-wal", "-shm"] {
        let p = db_path.with_file_name(format!(
            "{}{suffix}",
            db_path.file_name().unwrap_or_default().to_string_lossy()
        ));
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// List all saved connections, ordered by name.
pub async fn list(pool: &SqlitePool) -> Result<Vec<Connection>, StoreError> {
    Ok(sqlx::query_as::<_, Connection>(
        "SELECT id, name, host, port, default_db, username, ssl_mode, slot_budget, \
                password_ref, credential_source, bao_role_path, created_at \
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
                password_ref, credential_source, bao_role_path, created_at \
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
                                  slot_budget, password_ref, credential_source, bao_role_path) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING id, name, host, port, default_db, username, ssl_mode, slot_budget, \
                   password_ref, credential_source, bao_role_path, created_at",
    )
    .bind(&c.name)
    .bind(&c.host)
    .bind(c.port)
    .bind(&c.default_db)
    .bind(&c.username)
    .bind(&c.ssl_mode)
    .bind(c.slot_budget)
    .bind(&c.password_ref)
    .bind(&c.credential_source)
    .bind(&c.bao_role_path)
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

pub async fn update(
    pool: &SqlitePool,
    id: i64,
    c: NewConnection,
) -> Result<Connection, StoreError> {
    Ok(sqlx::query_as::<_, Connection>(
        "UPDATE connections SET \
            name = ?, host = ?, port = ?, default_db = ?, username = ?, \
            ssl_mode = ?, slot_budget = ?, password_ref = ?, \
            credential_source = ?, bao_role_path = ? \
         WHERE id = ? \
         RETURNING id, name, host, port, default_db, username, ssl_mode, \
                     slot_budget, password_ref, credential_source, bao_role_path, created_at",
    )
    .bind(&c.name)
    .bind(&c.host)
    .bind(c.port)
    .bind(&c.default_db)
    .bind(&c.username)
    .bind(&c.ssl_mode)
    .bind(c.slot_budget)
    .bind(&c.password_ref)
    .bind(&c.credential_source)
    .bind(&c.bao_role_path)
    .bind(id)
    .fetch_one(pool)
    .await?)
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
            credential_source: "password".into(),
            bao_role_path: None,
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
}
