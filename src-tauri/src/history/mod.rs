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
            "SELECT id, ts, server_id, database, sql, duration_ms, ok, error \
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
            "SELECT id, ts, server_id, database, sql, duration_ms, ok, error \
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
    sqlx::query("DELETE FROM query_history")
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

        let a_only = list(&pool, 10, HistoryFilter { server_id: Some(a) })
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
