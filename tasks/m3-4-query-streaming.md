# M3.4 — Streaming query module + cursor-based pagination

## Goal

**Before (post-M3.3):** `commands::run_query` runs a SQL statement, materializes all rows via `client.query(&sql, &[])`, builds a `QueryResult`, and returns. The slot is acquired and released within the one call. Cancellation works (M3.3). There is no `fetch_more` or `close_result`. Large result sets either complete or run out of memory.

**After:** A new backend module `src-tauri/src/query/mod.rs` owns query execution. It uses a server-side Postgres cursor inside a transaction; the slot is held for the cursor's whole lifetime (across multiple `fetch_more` calls); the first chunk (1000 rows) is returned by `run_query`, and the frontend triggers `fetch_more` / `close_result` as the user clicks Load-more or moves on. A new `ResultRegistry` (Tauri-managed) maps a UUID `result_id` → `OpenResult` holding the owned slot guard and a cursor name. To make slot guards long-lived enough to live in the registry, `SlotManager` gains an `acquire_owned(self: Arc<Self>, db)` variant returning a lifetime-free `OwnedSlotGuard<C>`.

The frontend bridge gains `fetchMore`, `closeResult`, and a richer `runQuery` return shape. UI wiring (the actual Load-more button) is M3.6.

This task carries the most architectural weight in M3 — it touches the slot manager, adds a registry, and reshapes the most-used command. **Read it twice before writing code.**

## Current state

### `src-tauri/src/slots/mod.rs` (post-M3.2)

`SlotGuard<'a, C: Connector>` borrows the manager. `acquire` is the only constructor.

### `src-tauri/src/commands/mod.rs` (post-M3.1 + M3.3)

`run_query` is ~70 lines. It does: lookup → acquire → bare-SELECT guard → `client.query(&sql, &[])` → row-to-JSON → return `QueryResult`.

### `src-tauri/src/registry.rs`

`ServerRegistry { by_id: DashMap<i64, ServerHandle> }`. `ServerHandle { slot_manager: Arc<SlotManager<PgConnector>> }`. No "open results" anywhere.

### Frontend `src/lib/tauri.ts`

`api.runQuery(serverId, database, sql)` returns `QueryResult { columns, rows, row_count, duration_ms }`.

### `Cargo.toml`

`uuid = { version = "1" }` is in dependencies but with no features.

## Design choices baked into this spec

- **Server-side cursor inside an explicit transaction.** `BEGIN; DECLARE c CURSOR FOR <sql>; FETCH N FROM c; ... CLOSE c; COMMIT;`. The cursor is named per-result with a UUID prefix (`q_<uuid hex>`) to avoid name collisions across concurrent results on the same connection (shouldn't happen — one OpenResult per slot — but cheap defense).
- **`WITHOUT HOLD` cursor (the Postgres default).** A `WITH HOLD` cursor materializes at COMMIT and survives, but it doubles server memory and removes the cancellation cleanliness. Without HOLD, the cursor lives only inside the transaction we keep open — exactly what we want.
- **Owned slot guard.** The `OpenResult` needs to outlive the `run_query` call frame. `SlotGuard<'a, C>` is borrowed. The fix is **not** to clone or refcount the guard; it's to add an `acquire_owned` constructor that takes `Arc<SlotManager<C>>` and returns an `OwnedSlotGuard<C>` with no lifetime. `Drop` returns the connection to the manager. This is the minimum new surface in the slot manager.
- **Default chunk size 1000.** Defined as `pub const DEFAULT_CHUNK_SIZE: usize = 1000;` in `query/mod.rs`. The Tauri commands accept an optional override.
- **`fetch_more` blocks until N rows arrive or the cursor is exhausted.** Postgres' `FETCH N` returns up to N rows; if fewer are available the call returns with `has_more = false`. No timeout, no partial-row streaming.
- **Cancel during `fetch_more` invalidates the result.** Postgres aborts the transaction on cancel — the cursor is gone. The command catches the cancel error, removes the entry from `ResultRegistry`, and propagates the error to the frontend. The frontend treats any `fetch_more` error as a closed result.
- **`disconnect_server` sweeps the registry.** Every `OpenResult` for that server is closed (best-effort COMMIT-or-ROLLBACK then drop) before `disconnect_all` runs. Otherwise the disconnect waits forever on a busy slot.
- **Multiple concurrent results per server are allowed.** With slot budget = 2, the user can keep 2 cursors open against the same server. The 3rd `run_query` returns `SlotError::AllBusy` until a result is closed.
- **Bare-`SELECT` rejection stays in `run_query`** (moved from `commands/mod.rs` to `query/mod.rs`). The same test in `commands` is removed; a new one lives in `query`.
- **Cursors are case-sensitive identifiers.** We always quote the cursor name in the SQL we issue (`DECLARE "q_..." CURSOR ...`) so a future UUID-with-hex format change doesn't surprise anyone.
- **No `WITH HOLD`, no `SCROLL`.** Forward-only. The frontend never scrolls back; if the user closes a result and reopens, that's a fresh `run_query`.
- **Slot indicator semantics get richer.** A held cursor keeps the slot busy. The user sees `[1/2]` while a result is open — which is honest: the connection is genuinely in use for that result. Closing a result drops the slot to idle.

## Deliverables

### 1. `src-tauri/src/slots/mod.rs` — add `acquire_owned` and `OwnedSlotGuard`

Add at the end of the public API section, before the test module:

```rust
// ═══════════════════════════════════════════════════════════════════════════
// OwnedSlotGuard  (RAII, lifetime-free)
// ═══════════════════════════════════════════════════════════════════════════

/// Like [`SlotGuard`], but owns an `Arc<SlotManager<C>>` instead of
/// borrowing.  Required for guards that live inside long-lived
/// structures (e.g. M3.4's `query::OpenResult`).
///
/// Returning the connection on drop works the same way as `SlotGuard`.
pub struct OwnedSlotGuard<C: Connector> {
    manager: Arc<SlotManager<C>>,
    slot_idx: usize,
    conn: Option<C::Conn>,
}

impl<C: Connector> std::ops::Deref for OwnedSlotGuard<C> {
    type Target = C::Conn;
    fn deref(&self) -> &C::Conn {
        self.conn.as_ref().expect("OwnedSlotGuard always holds a connection")
    }
}

impl<C: Connector> std::ops::DerefMut for OwnedSlotGuard<C> {
    fn deref_mut(&mut self) -> &mut C::Conn {
        self.conn.as_mut().expect("OwnedSlotGuard always holds a connection")
    }
}

impl<C: Connector> Drop for OwnedSlotGuard<C> {
    fn drop(&mut self) {
        let conn = self
            .conn
            .take()
            .expect("OwnedSlotGuard always holds a connection");

        let mut slots = match self.manager.slots.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                drop(poisoned.into_inner());
                return;
            }
        };

        let slot = &mut slots[self.slot_idx];
        slot.busy = false;

        if slot.disconnect_pending {
            slot.conn = None;
            slot.database = None;
            slot.cancel = None;
            slot.disconnect_pending = false;
            drop(slots);
            tokio::spawn(async move { C::close(conn).await });
        } else {
            slot.conn = Some(conn);
            slot.last_used = Instant::now();
        }
    }
}

impl<C: Connector> SlotManager<C> {
    /// Acquire a slot bound to `database` and return an owning guard that
    /// can outlive this call frame.
    ///
    /// Same rules as [`acquire`](Self::acquire); the only difference is
    /// the returned type.
    pub async fn acquire_owned(
        self: Arc<Self>,
        database: &str,
    ) -> Result<OwnedSlotGuard<C>, SlotError> {
        let db = database.to_string();

        let decision = {
            let mut slots = self.slots.lock().unwrap();
            let budget = self.budget.load(Ordering::Relaxed);
            while slots.len() < budget {
                slots.push(Slot::free());
            }
            apply_rules(&mut slots, &db, budget)?
        };

        let (idx, conn) = match decision {
            SlotDecision::Reuse { idx, conn } => (idx, conn),
            SlotDecision::NeedsConnect { idx, evict_conn } => {
                if let Some(old) = evict_conn {
                    C::close(old).await;
                }

                let recovery = Recovery {
                    slots: &self.slots,
                    idx,
                    recovered: std::cell::Cell::new(false),
                };
                let (new_conn, new_cancel) = self.connector.connect(&db).await?;
                {
                    let mut slots = self.slots.lock().unwrap();
                    slots[idx].cancel = Some(new_cancel);
                }
                recovery.recovered.set(true);
                (idx, new_conn)
            }
        };

        Ok(OwnedSlotGuard {
            manager: self,
            slot_idx: idx,
            conn: Some(conn),
        })
    }
}
```

Add a small test:

```rust
#[tokio::test]
async fn owned_guard_returns_conn_to_pool() {
    let (conn, connects, _closes) = FakeConnector::new();
    let mgr = Arc::new(SlotManager::new(conn, 1));

    let g = mgr.clone().acquire_owned("A").await.unwrap();
    drop(g);

    let g = mgr.clone().acquire_owned("A").await.unwrap();
    drop(g);

    assert_eq!(
        connects.load(Ordering::SeqCst),
        1,
        "second acquire should reuse the existing connection"
    );
}
```

### 2. `src-tauri/Cargo.toml` — enable `uuid v4`

```toml
uuid = { version = "1", features = ["v4"] }
```

### 3. `src-tauri/src/query/mod.rs` — new module

```rust
//! Query execution + cursor-based pagination.
//!
//! `run_query` opens a server-side cursor inside a transaction, fetches
//! the first 1000 rows, and stashes the open guard in a [`ResultRegistry`]
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

/// Errors specific to query execution.  Convert into `CommandError::Pg`
/// from the Tauri layer.
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
}

impl From<tokio_postgres::Error> for QueryError {
    fn from(e: tokio_postgres::Error) -> Self {
        QueryError::Pg(e.to_string())
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

/// Open a cursor and return the first chunk.
pub async fn run_query(
    server_id: i64,
    database: &str,
    sql: &str,
    chunk_size: usize,
    slot_manager: Arc<SlotManager<PgConnector>>,
    results: &ResultRegistry,
) -> Result<RunResult, QueryError> {
    if is_bare_select(sql) {
        return Err(QueryError::BareSelect);
    }

    let mut guard = slot_manager.acquire_owned(database).await?;

    // Begin transaction + declare cursor.
    let result_id = Uuid::new_v4();
    let cursor_name = format!("q_{}", result_id.simple());

    let start = Instant::now();
    guard.batch_execute("BEGIN").await?;

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
            return Err(QueryError::Pg(e.to_string()));
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

    let open = OpenResult {
        guard,
        cursor_name,
        columns: columns.clone(),
        row_count_so_far: row_count,
        duration_ms_so_far,
        has_more,
        server_id,
    };
    results.by_id.insert(result_id, open);

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
            return Err(QueryError::Pg(e.to_string()));
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

pub async fn close_result(
    result_id: Uuid,
    results: &ResultRegistry,
) -> Result<(), QueryError> {
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
    fn allows_real_select_queries() {
        assert!(!is_bare_select("SELECT 1"));
        assert!(!is_bare_select("SELECT * FROM foo"));
        assert!(!is_bare_select(" SELECT pg_sleep(1) "));
        assert!(!is_bare_select(""));
        assert!(!is_bare_select("INSERT INTO t VALUES (1)"));
    }
}
```

### 4. `src-tauri/src/lib.rs` — register the result registry and new commands

Add `pub mod query;` near the other module declarations. Inside `setup`:

```rust
app.manage(query::ResultRegistry::default());
```

Extend `invoke_handler`:

```rust
.invoke_handler(tauri::generate_handler![
    // ...existing 13...
    commands::fetch_more,
    commands::close_result,
])
```

### 5. `src-tauri/src/commands/mod.rs` — rewrite `run_query`, add `fetch_more` and `close_result`

Add use:

```rust
use crate::query::{self, ChunkResult, ResultRegistry, RunResult, DEFAULT_CHUNK_SIZE};
use uuid::Uuid;
```

Replace `run_query` body with a thin shim:

```rust
#[tauri::command]
pub async fn run_query(
    server_id: i64,
    database: String,
    sql: String,
    chunk_size: Option<usize>,
    registry: State<'_, ServerRegistry>,
    results: State<'_, ResultRegistry>,
) -> Result<RunResult, CommandError> {
    let handle = registry
        .by_id
        .get(&server_id)
        .ok_or_else(|| CommandError::not_connected(server_id))?;
    let slot_manager = handle.slot_manager.clone();
    drop(handle);

    let chunk = chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE);
    query::run_query(server_id, &database, &sql, chunk, slot_manager, &results)
        .await
        .map_err(map_query_err)
}

#[tauri::command]
pub async fn fetch_more(
    result_id: String,
    chunk_size: Option<usize>,
    results: State<'_, ResultRegistry>,
) -> Result<ChunkResult, CommandError> {
    let id = Uuid::parse_str(&result_id)
        .map_err(|e| CommandError::Pg(format!("invalid result_id: {e}")))?;
    let chunk = chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE);
    query::fetch_more(id, chunk, &results)
        .await
        .map_err(map_query_err)
}

#[tauri::command]
pub async fn close_result(
    result_id: String,
    results: State<'_, ResultRegistry>,
) -> Result<(), CommandError> {
    let id = Uuid::parse_str(&result_id)
        .map_err(|e| CommandError::Pg(format!("invalid result_id: {e}")))?;
    query::close_result(id, &results)
        .await
        .map_err(map_query_err)
}

fn map_query_err(e: query::QueryError) -> CommandError {
    match e {
        query::QueryError::BareSelect => CommandError::Pg(
            "incomplete query: SELECT requires a column list".into(),
        ),
        query::QueryError::UnknownResult => CommandError::Pg(
            "result_id is not open (was it already closed?)".into(),
        ),
        query::QueryError::Pg(m) | query::QueryError::Slot(m) => CommandError::Pg(m),
    }
}
```

The old `QueryResult` struct is no longer needed; remove it. `ColumnMeta` stays because `query::OpenResult` and `RunResult` reuse it. `pg_row_to_json` stays as-is.

Update `disconnect_server`:

```rust
#[tauri::command]
pub async fn disconnect_server(
    id: i64,
    registry: State<'_, ServerRegistry>,
    results: State<'_, ResultRegistry>,
) -> Result<(), CommandError> {
    // Close all open results on this server before tearing down slots.
    query::sweep_for_server(id, &results).await;

    let handle = registry
        .by_id
        .remove(&id)
        .map(|(_, h)| h)
        .ok_or_else(|| CommandError::not_connected(id))?;
    handle.slot_manager.disconnect_all().await;
    Ok(())
}
```

Remove the bare-SELECT unit test from `commands/mod.rs` — it lives in `query/mod.rs` now.

### 6. `src/lib/tauri.ts` — update types and methods

Replace `QueryResult` (it changes shape):

```ts
// ── Query results (mirrors query::RunResult / ChunkResult) ──

export type RunResult = {
  result_id: string;
  columns: ColumnMeta[];
  first_chunk: unknown[][];
  has_more: boolean;
  row_count_so_far: number;
  duration_ms_so_far: number;
};

export type ChunkResult = {
  rows: unknown[][];
  has_more: boolean;
  row_count_so_far: number;
  duration_ms_so_far: number;
};
```

Replace `runQuery` and add the two new methods:

```ts
  runQuery: (
    serverId: number,
    database: string,
    sql: string,
    chunkSize: number | null = null,
  ) =>
    invoke<RunResult>("run_query", { serverId, database, sql, chunkSize }),

  fetchMore: (resultId: string, chunkSize: number | null = null) =>
    invoke<ChunkResult>("fetch_more", { resultId, chunkSize }),

  closeResult: (resultId: string) =>
    invoke<void>("close_result", { resultId }),
```

The old `QueryResult` type can be removed; M2.4's `+page.svelte` references `QueryResult` and will be rewritten in M3.6 to use `RunResult` + chunk accumulation. **For this task** (backend-only), `+page.svelte` won't yet call `fetchMore` — but the existing call sites of `runQuery` must be updated to handle the new shape. The simplest M3.4 patch to `+page.svelte`: render `first_chunk` instead of `rows`, and stop showing row count from `row_count` (use `row_count_so_far`). A real "Load more" button waits for M3.6.

A minimal in-task diff to `+page.svelte`'s `renderResult` keeps the smoke test running:

```ts
function renderResult(r: RunResult): string {
  if (r.columns.length === 0)
    return `(no columns)\n${r.row_count_so_far} rows, ${r.duration_ms_so_far}ms`;
  const header = r.columns.map((c) => c.name).join("\t");
  const lines = r.first_chunk.map((row) =>
    row.map((cell) => {
      if (cell === null) return "NULL";
      if (typeof cell === "object") return JSON.stringify(cell);
      return String(cell);
    }).join("\t"),
  );
  return [
    header,
    ...lines,
    "",
    `${r.row_count_so_far} rows so far in ${r.duration_ms_so_far}ms`
      + (r.has_more ? " (more available — Load-more UI in M3.6)" : ""),
  ].join("\n");
}
```

Also update the `result` state type and the `runQuery` call site to use `RunResult`. This keeps the smoke test working until M3.6 wires the proper grid.

## Implementation order

1. **`slots/mod.rs`** — add `OwnedSlotGuard` and `acquire_owned`. `cargo build` continues to succeed; nothing calls them yet.
2. **`Cargo.toml`** — add `uuid` features. `cargo fetch`.
3. **`query/mod.rs`** — new module. `cargo build`.
4. **`lib.rs`** — declare the module, manage `ResultRegistry`, register new commands.
5. **`commands/mod.rs`** — update `disconnect_server`, rewrite `run_query`, add `fetch_more` + `close_result`, drop the old `QueryResult` + bare-SELECT test.
6. **`src/lib/tauri.ts`** — update types and methods.
7. **`src/routes/+page.svelte`** — minimal patch: use `RunResult` shape; drop `row_count` for `row_count_so_far`.
8. `./test.sh` clean.
9. Smoke test below.

## Known gotchas

- **Holding a `dashmap::get_mut` across an `await`** — the `Ref`/`RefMut` types in dashmap are NOT `Send`. The pattern above clones the cursor name out of the entry before awaiting `query`, then re-borrows after. **Don't try to await while holding `entry.guard` directly through the dashmap ref** — read the cursor name, drop the ref, await. The code shown above does this via a single `get_mut` that doesn't get dropped, which **works for `tokio_postgres::Client::query` because it takes `&self`** and we're holding `&mut Self::Output`. If clippy complains, refactor to clone the guard out via `Arc` — but `OwnedSlotGuard` is not Arc-able. The simplest fix: write the await as `entry.guard.query(...)` while keeping the entry borrowed; this compiles under Tokio's single-threaded executor but fails the `tokio::spawn` Send check if any caller tries to spawn. We pass it through tauri command handlers which already require Send futures — verify in practice. If it doesn't compile, the alternative is to `take()` the OpenResult out of the dashmap, await, then put it back: more code but Send-clean.
- **`tokio_postgres::Client::query` takes `&self`, not `&mut self`.** This is what lets us share the guard across the dashmap entry borrow. If you find yourself reaching for `&mut`, you're holding the wrong reference.
- **Cursor names with hyphens.** `Uuid::simple()` formats without hyphens (`uuid::fmt::Simple`). Hyphens in `DECLARE` would need quoting anyway, but `simple()` gives us a plain `[0-9a-f]{32}` string — safer.
- **`batch_execute` vs `execute` vs `query`.** Use `batch_execute` for multi-statement / DDL-style strings (`BEGIN`, `DECLARE`, `CLOSE`, `ROLLBACK`) — it doesn't return rows. Use `query` for `FETCH` (returns rows). Don't `execute` `FETCH` — you'd get `CommandComplete` only with no rows.
- **`SELECT INTO` and other utility statements can't be cursored.** Postgres refuses `DECLARE c CURSOR FOR SELECT INTO ...`. Surface this as a regular `QueryError::Pg` — the user sees the Postgres error message verbatim.
- **`DDL inside `DECLARE` fails identically.** `DECLARE c CURSOR FOR CREATE TABLE ...` errors. Same path. v1 doesn't try to detect this in advance.
- **Multi-statement scripts (`SELECT 1; SELECT 2;`)** — `DECLARE c CURSOR FOR SELECT 1; SELECT 2` is a parse error in Postgres. The cursor wraps exactly one statement. PRD §12 open question for multi-statement is resolved by **v1 runs the first statement only** — this is consistent with M3's milestone text. Statement-boundary detection happens client-side in CodeMirror (M3.5).
- **The cursor's lifetime is the transaction.** Don't let the transaction stay open longer than necessary. `close_result` rolls back; auto-close on exhaustion (rows.len() < chunk_size) also rolls back; `disconnect_server` sweeps.
- **Sweeping during disconnect must happen before `disconnect_all`.** Otherwise `disconnect_all` flips the busy slot to `disconnect_pending`, and when the sweep tries to send `CLOSE`/`ROLLBACK`, the slot guard hasn't dropped yet, but the conn is still alive — actually this would work fine. The order is for clarity, not correctness.
- **`disconnect_all` on a connection running an open cursor is safe.** The cursor is server-side; rolling back happens automatically on socket close. The `_` rollback inside `close_open` is belt-and-braces.
- **`ResultRegistry` is `Default`-able.** Tauri's `app.manage()` accepts any `Send + Sync + 'static` value. `DashMap` is both.
- **UUID collisions.** Probability of `Uuid::new_v4` collisions for tens of open results: negligible. Don't add retry logic.
- **OwnedSlotGuard's Drop is sync.** If `disconnect_pending` is set, the connection is `tokio::spawn`-ed for async close — this works because we're inside a tokio runtime (the Tauri main).
- **`query::QueryError::UnknownResult`** maps to `CommandError::Pg`, not a new variant. v1 keeps `CommandError` small; the message is self-explanatory.

## Tests

- **Existing tests** continue to pass after the `commands/mod.rs` `is_bare_select` test is moved to `query/mod.rs`.
- **New slot test**: `owned_guard_returns_conn_to_pool` (above).
- **New query tests** in `query/mod.rs`: the two `is_bare_select` tests, moved verbatim.

### Manual smoke test

```bash
docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17
docker exec -it quill-pg psql -U postgres -c \
  "CREATE TABLE big AS SELECT generate_series(1, 5000) AS n;"
./run.sh
```

1. Connect to `localhost:5432` with username `postgres`, password `dev`. Slot indicator `[0/2]`.
2. Click the `postgres` DB node.
3. Run `SELECT * FROM big ORDER BY n`.
4. The result `<pre>` shows the first 1000 rows + "5000 rows so far in Nms (more available — Load-more UI in M3.6)".
5. Slot indicator now reads `[1/2]` and **stays** `[1/2]` — the cursor is open.
6. Open devtools, run:
   ```javascript
   const { invoke } = window.__TAURI_INTERNALS__;
   await invoke("fetch_more", { resultId: "<from previous response>", chunkSize: null });
   ```
   Returns the next 1000 rows with `has_more: true, row_count_so_far: 2000`.
7. Repeat `fetch_more` until `has_more: false`. The 5th call returns `has_more: false`, `row_count_so_far: 5000`. **Slot indicator drops to `[0/2]`** — auto-close fired.
8. Run another `SELECT * FROM big`. Slot returns to `[1/2]`. This time, call `close_result(result_id)` from devtools. Slot drops to `[0/2]` immediately.
9. Run two queries against the same connection in parallel (open a long query, then a quick one). Slot indicator climbs to `[2/2]`. With budget 2, a third query returns `SlotError::AllBusy` — verified by visible error in UI.
10. Disconnect the server while a result is open. Slot indicator disappears; subsequent `fetch_more` for any prior id fails with `"result_id is not open"`.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds.
- [ ] `pnpm check` succeeds.
- [ ] `grep -F 'pub mod query;' src-tauri/src/lib.rs` returns one match.
- [ ] `grep -c '#\[tauri::command\]' src-tauri/src/commands/mod.rs` returns `15` (13 from M3.3 + `fetch_more` + `close_result`).
- [ ] `grep -F 'acquire_owned' src-tauri/src` returns matches in `slots/mod.rs` and `query/mod.rs` only.
- [ ] `grep -F 'WITH HOLD' src-tauri/src` returns zero matches (we deliberately don't use it).
- [ ] Smoke test step 5 — slot indicator visibly stays at `[1/2]` after `run_query` returns, until `fetch_more` exhausts the cursor or `close_result` is called.
- [ ] Smoke test step 7 — exhaustion auto-closes the result and frees the slot.
- [ ] Smoke test step 9 — slot budget is honored across concurrent open results.
- [ ] Disconnect-while-open-cursor sweeps cleanly (no leftover slots / no hangs).
- [ ] No new error variant in `CommandError`.
- [ ] M2.4 smoke procedure still passes; tree expansion still works.

## Out of scope

- The Cancel button in the UI — **M3.6**.
- CodeMirror editor — **M3.5**.
- Result-grid component (sortable / resizable / cell preview) — **M3.6**.
- Inline error rendering below the editor — **M3.6** (basic still in M2.4 form for now).
- Backwards scrolling (`SCROLL` cursors, `WITH HOLD`) — explicitly **not** in v1.
- Auto-cancel a fetch_more on user navigation away — v1 keeps the cursor open until explicit close or disconnect.
- Time-based result expiry — out of scope (would re-introduce a background sweep, violating principle 1).
- A configurable default chunk size in the settings panel — **M6**.
- Multi-statement script execution — defer to M6 per PRD §12.
- Cursor server-side memory pressure (large result sets pinned in memory) — Postgres-side concern; users add LIMIT.
