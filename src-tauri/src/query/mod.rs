//! Query execution + cursor-based pagination.
//!
//! `run_query` opens a server-side cursor inside a read-only transaction,
//! fetches the first 1000 rows, and stashes the open guard in a [`ResultRegistry`]
//! keyed by UUID.  Subsequent `fetch_more(result_id)` calls fetch the next
//! chunk; `close_result(result_id)` rolls back the transaction and drops
//! the guard.
//!
//! AGENTS.md principle 2: holding a slot for the lifetime of a result is
//! deliberate and the slot indicator should reflect it ([1/2] while a
//! result is open).

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use serde::Serialize;
use serde_json::Value;
use tokio_postgres::Row;
use uuid::Uuid;

use crate::commands::{ColumnMeta, pg_row_to_json};
use crate::pg::PgConnector;
use crate::slots::{OwnedSlotGuard, SlotError, SlotManager};

pub const DEFAULT_CHUNK_SIZE: usize = 1000;

/// One open server-side cursor.  Owns the slot guard for the result's
/// lifetime.  Dropping this struct closes the cursor *eventually* — the
/// transaction rollback runs when the connection is reused.  Prefer
/// [`close_result`] for explicit cleanup.
pub struct OpenResult {
    guard: OwnedSlotGuard<PgConnector>,
    cursor_name: String,
    pub columns: Vec<ColumnMeta>,
    /// Number of rows fetched so far across the cursor's lifetime.
    /// Updated after every `fetch_more`.
    pub row_count_so_far: usize,
    /// Cumulative milliseconds spent inside Postgres for this result.
    pub duration_ms_so_far: u64,
    /// Becomes `false` after a FETCH returns fewer rows than requested.
    pub has_more: bool,
    /// `server_id` from the connection registry — used by sweep on disconnect.
    pub server_id: i64,
}

/// Tauri-managed registry.  Empty at startup; entries created by
/// `run_query` and removed by `close_result` / disconnect sweep.
#[derive(Default)]
pub struct ResultRegistry {
    pub by_id: DashMap<Uuid, OpenResult>,
}

#[derive(Debug, Serialize)]
pub struct RunResult {
    pub result_id: String,
    pub columns: Vec<ColumnMeta>,
    pub first_chunk: Vec<Vec<Value>>,
    pub has_more: bool,
    pub row_count_so_far: usize,
    pub duration_ms_so_far: u64,
}

#[derive(Debug, Serialize)]
pub struct ChunkResult {
    pub rows: Vec<Vec<Value>>,
    pub has_more: bool,
    pub row_count_so_far: usize,
    pub duration_ms_so_far: u64,
}

/// Errors specific to query execution.  Mapped to `CommandError` variants
/// by `map_query_err` in the Tauri layer.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("incomplete query: SELECT requires a column list")]
    BareSelect,
    #[error("unknown result id")]
    UnknownResult,
    #[error("{0}")]
    Pg(String),
    #[error("{0}")]
    Slot(String),
    /// The server's connection budget is full — all `n` slots are busy.
    /// Distinct from `Slot` so the frontend can offer cancel/retry actions.
    #[error("all {0} connections are in use")]
    Budget(usize),
}

impl From<tokio_postgres::Error> for QueryError {
    fn from(e: tokio_postgres::Error) -> Self {
        QueryError::Pg(crate::pg::format_pg_error(&e))
    }
}
impl From<SlotError> for QueryError {
    fn from(e: SlotError) -> Self {
        QueryError::Slot(e.to_string())
    }
}

/// Reject queries that are *just* the word SELECT.
fn is_bare_select(sql: &str) -> bool {
    let bare = sql.trim().trim_matches(';').trim();
    bare.eq_ignore_ascii_case("SELECT")
}

/// Wrap an identifier in double quotes, doubling any internal quote so a
/// schema name with odd characters can't break out of the `SET search_path`
/// statement.
fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Open a cursor and return the first chunk.
pub async fn run_query(
    server_id: i64,
    database: &str,
    sql: &str,
    schema: Option<&str>,
    chunk_size: usize,
    slot_manager: Arc<SlotManager<PgConnector>>,
    results: &ResultRegistry,
) -> Result<RunResult, QueryError> {
    if is_bare_select(sql) {
        return Err(QueryError::BareSelect);
    }

    let guard = match slot_manager.acquire_owned(database).await {
        Ok(g) => g,
        Err(SlotError::AllBusy(n)) => return Err(QueryError::Budget(n)),
        Err(e) => return Err(e.into()),
    };

    // Begin transaction + declare cursor.
    let result_id = Uuid::new_v4();
    let cursor_name = format!("q_{}", result_id.simple());

    let start = Instant::now();
    // READ ONLY makes v1's read-only guarantee explicit: Postgres rejects any
    // write outright, rather than relying on the cursor wrapper (which only
    // accepts SELECT/VALUES) plus the final ROLLBACK to discard it.
    guard.batch_execute("BEGIN READ ONLY").await?;

    // Scope unqualified names to a single schema when the editor was opened
    // against a schema node. `SET LOCAL` is confined to this transaction, so
    // it never leaks onto the next query that reuses the slot.
    if let Some(schema) = schema {
        let set_sql = format!(r#"SET LOCAL search_path TO {}"#, quote_ident(schema));
        if let Err(e) = guard.batch_execute(&set_sql).await {
            let _ = guard.batch_execute("ROLLBACK").await;
            return Err(QueryError::Pg(crate::pg::format_pg_error(&e)));
        }
    }

    // Declare the cursor — interpolate the cursor name (we control it) and
    // the user SQL (we don't; pass it verbatim and trust Postgres parsing).
    let decl_sql = format!(r#"DECLARE "{cursor_name}" CURSOR FOR {sql}"#);
    if let Err(e) = guard.batch_execute(&decl_sql).await {
        // Best-effort rollback before bubbling up.
        let _ = guard.batch_execute("ROLLBACK").await;
        return Err(QueryError::Pg(e.to_string()));
    }

    let fetch_sql = format!(r#"FETCH {chunk_size} FROM "{cursor_name}""#);
    let rows: Vec<Row> = match guard.query(&fetch_sql, &[]).await {
        Ok(rs) => rs,
        Err(e) => {
            let _ = guard.batch_execute("ROLLBACK").await;
            return Err(QueryError::Pg(crate::pg::format_pg_error(&e)));
        }
    };
    let duration_ms_so_far = start.elapsed().as_millis() as u64;

    let columns: Vec<ColumnMeta> = rows
        .first()
        .map(|r| {
            r.columns()
                .iter()
                .map(|col| ColumnMeta {
                    name: col.name().to_string(),
                    type_name: col.type_().name().to_uppercase(),
                })
                .collect()
        })
        .unwrap_or_default();

    let first_chunk: Vec<Vec<Value>> = rows.iter().map(pg_row_to_json).collect();
    let row_count = first_chunk.len();
    let has_more = row_count == chunk_size;

    if has_more {
        results.by_id.insert(
            result_id,
            OpenResult {
                guard,
                cursor_name,
                columns: columns.clone(),
                row_count_so_far: row_count,
                duration_ms_so_far,
                has_more,
                server_id,
            },
        );
    } else {
        let mut open = OpenResult {
            guard,
            cursor_name,
            columns: columns.clone(),
            row_count_so_far: row_count,
            duration_ms_so_far,
            has_more,
            server_id,
        };
        let _ = close_open(&mut open).await;
    }

    Ok(RunResult {
        result_id: result_id.to_string(),
        columns,
        first_chunk,
        has_more,
        row_count_so_far: row_count,
        duration_ms_so_far,
    })
}

pub async fn fetch_more(
    result_id: Uuid,
    chunk_size: usize,
    results: &ResultRegistry,
) -> Result<ChunkResult, QueryError> {
    // Hold the dashmap entry for the duration so concurrent close_result
    // doesn't yank the OpenResult mid-fetch.
    let mut entry = results
        .by_id
        .get_mut(&result_id)
        .ok_or(QueryError::UnknownResult)?;

    let cursor_name = entry.cursor_name.clone();
    let fetch_sql = format!(r#"FETCH {chunk_size} FROM "{cursor_name}""#);

    let start = Instant::now();
    let rows: Vec<Row> = match entry.guard.query(&fetch_sql, &[]).await {
        Ok(rs) => rs,
        Err(e) => {
            // Cursor / transaction is dead.  Drop the entry so the slot
            // releases.  Borrow checker: we hold a `get_mut` reference; we
            // can't remove from inside, so collect the id and drop after.
            drop(entry);
            results.by_id.remove(&result_id);
            return Err(QueryError::Pg(crate::pg::format_pg_error(&e)));
        }
    };
    entry.duration_ms_so_far += start.elapsed().as_millis() as u64;

    let json_rows: Vec<Vec<Value>> = rows.iter().map(pg_row_to_json).collect();
    let n = json_rows.len();
    entry.row_count_so_far += n;
    entry.has_more = n == chunk_size;

    let result = ChunkResult {
        rows: json_rows,
        has_more: entry.has_more,
        row_count_so_far: entry.row_count_so_far,
        duration_ms_so_far: entry.duration_ms_so_far,
    };

    if !entry.has_more {
        // Auto-close exhausted cursors so the slot releases immediately.
        drop(entry);
        if let Some((_, mut open)) = results.by_id.remove(&result_id) {
            let _ = close_open(&mut open).await;
        }
    }

    Ok(result)
}

pub async fn close_result(result_id: Uuid, results: &ResultRegistry) -> Result<(), QueryError> {
    if let Some((_, mut open)) = results.by_id.remove(&result_id) {
        close_open(&mut open).await?;
    }
    Ok(())
}

/// Internal: close the cursor and rollback the transaction.  Errors are
/// downgraded — closing is best-effort.
async fn close_open(open: &mut OpenResult) -> Result<(), QueryError> {
    let close_sql = format!(r#"CLOSE "{}""#, open.cursor_name);
    let _ = open.guard.batch_execute(&close_sql).await;
    let _ = open.guard.batch_execute("ROLLBACK").await;
    // The OwnedSlotGuard drops naturally when `open` is dropped after this.
    Ok(())
}

/// Sweep all open results for `server_id` and close them.  Called by
/// `disconnect_server` before `disconnect_all`.
pub async fn sweep_for_server(server_id: i64, results: &ResultRegistry) {
    let ids: Vec<Uuid> = results
        .by_id
        .iter()
        .filter(|e| e.value().server_id == server_id)
        .map(|e| *e.key())
        .collect();

    for id in ids {
        if let Some((_, mut open)) = results.by_id.remove(&id) {
            let _ = close_open(&mut open).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bare_select_variants() {
        assert!(is_bare_select("SELECT"));
        assert!(is_bare_select("SELECT "));
        assert!(is_bare_select("  SELECT  "));
        assert!(is_bare_select("select"));
        assert!(is_bare_select("SELECT;"));
        assert!(is_bare_select("SELECT ;;;"));
    }

    #[test]
    fn quote_ident_escapes_embedded_quotes() {
        assert_eq!(quote_ident("public"), r#""public""#);
        assert_eq!(quote_ident("my.schema"), r#""my.schema""#);
        assert_eq!(quote_ident(r#"we"ird"#), r#""we""ird""#);
    }

    #[test]
    fn allows_real_select_queries() {
        assert!(!is_bare_select("SELECT 1"));
        assert!(!is_bare_select("SELECT * FROM foo"));
        assert!(!is_bare_select(" SELECT pg_sleep(1) "));
        assert!(!is_bare_select(""));
        assert!(!is_bare_select("INSERT INTO t VALUES (1)"));
    }
}
