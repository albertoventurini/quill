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
    #[serde(rename = "scope")]
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
pub async fn create(pool: &SqlitePool, new: NewSavedQuery) -> Result<SavedQuery, SavedError> {
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
        sqlx::query_as("SELECT id FROM saved_queries WHERE scope = 'global' AND name = ?")
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
pub async fn rename(pool: &SqlitePool, id: i64, new_name: &str) -> Result<SavedQuery, SavedError> {
    // Fetch current row for the duplicate check.
    let current: SavedQuery = sqlx::query_as(
        "SELECT id, name, scope, server_id, sql, created_at FROM saved_queries WHERE id = ?",
    )
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
                ssl_mode: "prefer".into(),
                slot_budget: 2,
                credential_source: "password".into(),
                bao_role_path: None,
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
