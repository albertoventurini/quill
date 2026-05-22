//! Tauri command surface — the IPC bridge between frontend and backend.
//!
//! Every command that needs a database connection goes through the
//! `ServerRegistry` (which holds per-server `SlotManager`s).  No command
//! opens a Postgres connection eagerly — connections happen only inside
//! `run_query` (AGENTS.md principle 1).

use secrecy::SecretString;
use serde::Serialize;
use serde_json::Value;
use tauri::State;
use tokio_postgres::Row;
use tokio_postgres::types::Type;

use crate::pg::PgConnector;
use crate::query::{self, ChunkResult, DEFAULT_CHUNK_SIZE, ResultRegistry, RunResult};
use crate::registry::{ServerHandle, ServerRegistry};
use crate::slots::{SlotError, SlotState};
use crate::store;
use uuid::Uuid;

// ═══════════════════════════════════════════════════════════════════════════
// Command error type  (serialized as { kind, message })
// ═══════════════════════════════════════════════════════════════════════════

/// Every command returns `Result<_, CommandError>`.  The serde tagging
/// produces `{"kind": "Pg", "message": "..."}` so the frontend can
/// branch on `kind` while always having a human-readable `message`.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum CommandError {
    UnknownConnection(String),
    NotConnected(String),
    Slot(String),
    Pg(String),
    Store(String),
    Introspect(String),
    Saved(String),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownConnection(msg)
            | Self::NotConnected(msg)
            | Self::Slot(msg)
            | Self::Pg(msg)
            | Self::Store(msg)
            | Self::Introspect(msg)
            | Self::Saved(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CommandError {}

// Convenience constructors
impl CommandError {
    fn unknown_connection(id: i64) -> Self {
        Self::UnknownConnection(format!("connection {id} not found"))
    }
    fn not_connected(id: i64) -> Self {
        Self::NotConnected(format!("not connected to server {id}"))
    }
}

impl From<store::StoreError> for CommandError {
    fn from(e: store::StoreError) -> Self {
        Self::Store(e.to_string())
    }
}

impl From<crate::introspect::IntrospectError> for CommandError {
    fn from(e: crate::introspect::IntrospectError) -> Self {
        Self::Introspect(e.to_string())
    }
}

impl From<crate::history::HistoryError> for CommandError {
    fn from(e: crate::history::HistoryError) -> Self {
        Self::Store(e.to_string())
    }
}

impl From<crate::saved::SavedError> for CommandError {
    fn from(e: crate::saved::SavedError) -> Self {
        Self::Saved(e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Query result types
// ═══════════════════════════════════════════════════════════════════════════

/// Metadata for one result-set column.
#[derive(Debug, Serialize, Clone)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
}

// ═══════════════════════════════════════════════════════════════════════════
// Row → JSON conversion
// ═══════════════════════════════════════════════════════════════════════════

/// Convert a single tokio-postgres `Row` into a `Vec<serde_json::Value>`.
///
/// Switches on the column's Postgres type.  Unrecognised types fall
/// back to a best-effort `&str` representation — this function **never
/// errors**; the user always sees something.
pub fn pg_row_to_json(row: &Row) -> Vec<Value> {
    let columns = row.columns();
    let mut values = Vec::with_capacity(columns.len());

    for (i, col) in columns.iter().enumerate() {
        let val = match *col.type_() {
            Type::BOOL => option_to_json(row.try_get::<_, Option<bool>>(i), Value::Bool),
            Type::INT2 => option_to_json(row.try_get::<_, Option<i16>>(i), |v| {
                Value::Number((v as i64).into())
            }),
            Type::INT4 => option_to_json(row.try_get::<_, Option<i32>>(i), |v| {
                Value::Number((v as i64).into())
            }),
            Type::INT8 => option_to_json(row.try_get::<_, Option<i64>>(i), |v| {
                Value::Number(v.into())
            }),
            Type::OID => option_to_json(row.try_get::<_, Option<u32>>(i), |v| {
                Value::Number((v as i64).into())
            }),
            Type::FLOAT4 => option_to_json(row.try_get::<_, Option<f32>>(i), |v| {
                serde_json::Number::from_f64(v as f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }),
            Type::FLOAT8 => option_to_json(row.try_get::<_, Option<f64>>(i), |v| {
                serde_json::Number::from_f64(v)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }),

            Type::JSON | Type::JSONB => option_to_json(row.try_get::<_, Option<Value>>(i), |v| v),

            Type::BYTEA => option_to_json(row.try_get::<_, Option<Vec<u8>>>(i), |bytes| {
                use base64::Engine;
                Value::String(base64::engine::general_purpose::STANDARD.encode(&bytes))
            }),

            Type::UUID => option_to_json(row.try_get::<_, Option<uuid::Uuid>>(i), |u| {
                Value::String(u.to_string())
            }),

            Type::DATE => option_to_json(row.try_get::<_, Option<chrono::NaiveDate>>(i), |d| {
                Value::String(d.to_string())
            }),
            Type::TIME => option_to_json(row.try_get::<_, Option<chrono::NaiveTime>>(i), |t| {
                Value::String(t.to_string())
            }),
            Type::TIMESTAMP => {
                option_to_json(row.try_get::<_, Option<chrono::NaiveDateTime>>(i), |t| {
                    Value::String(t.to_string())
                })
            }
            Type::TIMESTAMPTZ => option_to_json(
                row.try_get::<_, Option<chrono::DateTime<chrono::Utc>>>(i),
                |t| Value::String(t.to_rfc3339()),
            ),

            Type::NUMERIC => {
                option_to_json(row.try_get::<_, Option<rust_decimal::Decimal>>(i), |d| {
                    Value::String(d.to_string())
                })
            }

            // Text-family types
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::CHAR => {
                option_to_json(row.try_get::<_, Option<&str>>(i), |s| {
                    Value::String(s.to_string())
                })
            }

            // Unknown — best-effort &str fallback.
            _ => match row.try_get::<_, Option<&str>>(i) {
                Ok(Some(s)) => Value::String(s.to_string()),
                Ok(None) => Value::Null,
                Err(_) => Value::Null,
            },
        };
        values.push(val);
    }

    values
}

/// Tiny helper: collapse `Result<Option<T>, _>` into `serde_json::Value`
/// via a converter for the `Some` branch.  `Err` and `None` both become
/// `Value::Null` — the user always sees something.
fn option_to_json<T, F: FnOnce(T) -> Value>(
    r: Result<Option<T>, tokio_postgres::Error>,
    f: F,
) -> Value {
    match r {
        Ok(Some(v)) => f(v),
        Ok(None) | Err(_) => Value::Null,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tauri commands
// ═══════════════════════════════════════════════════════════════════════════

/// List all saved connections from the local store.
#[tauri::command]
pub async fn list_connections(
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<Vec<store::Connection>, CommandError> {
    Ok(store::list(&pool).await?)
}

/// Save a new connection to the local store.  Returns the saved row with
/// the auto-generated `id` and `created_at`.
#[tauri::command]
pub async fn save_connection(
    new: store::NewConnection,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<store::Connection, CommandError> {
    Ok(store::insert(&pool, new).await?)
}

/// Delete a saved connection by id.  Does nothing if the id doesn't exist.
#[tauri::command]
pub async fn delete_connection(
    id: i64,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<(), CommandError> {
    store::delete(&pool, id).await?;
    Ok(())
}

/// Connect to a saved server.
///
/// 1. Load the `Connection` from the store; return `UnknownConnection` if missing.
/// 2. Build a `PgConnector` from the row + the supplied password.
/// 3. If the registry already contains a `ServerHandle` for `id`, reuse it
///    (the password is ignored — the server is already running).
/// 4. Otherwise, create a new `SlotManager<PgConnector>` with the row's
///    `slot_budget` and insert it into the registry.
/// 5. Return the current `SlotState`.  **No Postgres connection is opened**
///    (AGENTS.md principle 1).
#[tauri::command]
pub async fn connect_server(
    id: i64,
    password: String,
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<SlotState, CommandError> {
    // Already connected?  Return current state immediately.
    if let Some(handle) = registry.by_id.get(&id) {
        return Ok(handle.slot_manager.state());
    }

    // Load the saved connection from the store.
    let conn = store::get(&pool, id)
        .await?
        .ok_or_else(|| CommandError::unknown_connection(id))?;

    // Build a PgConnector.
    let ssl_mode =
        PgConnector::parse_ssl_mode(&conn.ssl_mode).map_err(|e| CommandError::Pg(e.0))?;
    let connector = PgConnector {
        host: conn.host.clone(),
        port: conn.port as u16,
        username: conn.username.clone(),
        password: SecretString::from(password),
        ssl_mode,
    };

    let budget = conn.slot_budget.max(1) as usize;
    let handle = ServerHandle::new(connector, budget);
    let state = handle.slot_manager.state();

    registry.by_id.insert(id, handle);
    Ok(state)
}

/// Disconnect a server: remove it from the registry and close all its
/// slots (idle ones immediately; busy ones when their guards drop).
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

/// Run a SQL query against a connected server.  Opens a server-side cursor
/// and returns the first chunk (default 1000 rows).  Subsequent chunks are
/// fetched via [`fetch_more`]; the cursor is closed with [`close_result`].
///
/// Every call appends one row to `query_history` — on success with the
/// time-to-first-chunk and the row's `ok=true`; on failure with the time
/// elapsed before the error and `ok=false`.  History failures are logged
/// and swallowed — they never alter the response visible to the user.
#[tauri::command]
pub async fn run_query(
    server_id: i64,
    database: String,
    sql: String,
    chunk_size: Option<usize>,
    pool: State<'_, sqlx::SqlitePool>,
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

    let start = std::time::Instant::now();

    let outcome = query::run_query(server_id, &database, &sql, chunk, slot_manager, &results).await;

    let (record, response) = match outcome {
        Ok(run) => {
            let record = NewHistoryRecord {
                server_id,
                database: database.clone(),
                sql: sql.clone(),
                duration_ms: run.duration_ms_so_far as i64,
                ok: true,
                error: None,
            };
            (record, Ok(run))
        }
        Err(e) => {
            let err = map_query_err(e);
            let record = NewHistoryRecord {
                server_id,
                database: database.clone(),
                sql: sql.clone(),
                duration_ms: start.elapsed().as_millis() as i64,
                ok: false,
                error: Some(err.to_string()),
            };
            (record, Err(err))
        }
    };

    if let Err(history_err) = history::append(&pool, record).await {
        eprintln!("history::append failed: {history_err}");
    }

    response
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
        query::QueryError::BareSelect => {
            CommandError::Pg("incomplete query: SELECT requires a column list".into())
        }
        query::QueryError::UnknownResult => {
            CommandError::Pg("result_id is not open (was it already closed?)".into())
        }
        query::QueryError::Pg(m) | query::QueryError::Slot(m) => CommandError::Pg(m),
    }
}

/// Return the current slot state for a connected server.
///
/// Synchronous — just reads the snapshot, no I/O.
#[tauri::command]
pub fn get_slot_state(
    server_id: i64,
    registry: State<'_, ServerRegistry>,
) -> Result<Option<SlotState>, CommandError> {
    Ok(registry
        .by_id
        .get(&server_id)
        .map(|h| h.slot_manager.state()))
}

// ═══════════════════════════════════════════════════════════════════════════
// Schema-cache helpers
// ═══════════════════════════════════════════════════════════════════════════

use crate::introspect::{self, DatabaseInfo, FunctionInfo, RelationInfo, SchemaPayload};

/// Return the schema payload for `(server_id, database)` from the
/// session-scoped in-memory cache, or — on a miss — acquire a slot,
/// introspect, populate the cache, and return the fresh payload.
///
/// The cache lives on `ServerHandle` and is created empty when the user
/// connects; it is discarded on disconnect.  This means the data is always
/// fresh at the start of each session and never stale across restarts.
/// Within a session the first expand of a database populates the entry;
/// all subsequent calls for the same database are zero-slot-cost lookups.
async fn ensure_payload(
    server_id: i64,
    database: &str,
    registry: &ServerRegistry,
) -> Result<SchemaPayload, CommandError> {
    // Clone the Arc out of the DashMap shard lock before any await.
    let schema_cache = {
        let handle = registry
            .by_id
            .get(&server_id)
            .ok_or_else(|| CommandError::not_connected(server_id))?;
        handle.schema_cache.clone()
    };

    if let Some(payload) = schema_cache.get(database) {
        return Ok(payload.clone());
    }

    // Cache miss — introspect and populate.
    let payload = run_introspection(server_id, database, registry).await?;
    schema_cache.insert(database.to_string(), payload.clone());
    Ok(payload)
}

/// Acquire a slot bound to `database` on `server_id` and run a full
/// introspection.  Used by both `ensure_payload` (cache miss path) and
/// `refresh_schema_cache` (explicit refresh).
async fn run_introspection(
    server_id: i64,
    database: &str,
    registry: &ServerRegistry,
) -> Result<SchemaPayload, CommandError> {
    let handle = registry
        .by_id
        .get(&server_id)
        .ok_or_else(|| CommandError::not_connected(server_id))?;
    let slot_manager = handle.slot_manager.clone();
    drop(handle);

    let guard = slot_manager
        .acquire(database)
        .await
        .map_err(|e: SlotError| CommandError::Slot(e.to_string()))?;

    Ok(introspect::introspect_database(&guard).await?)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tauri commands — introspection surface
// ═══════════════════════════════════════════════════════════════════════════

/// List every connectable, non-template database on the server.
///
/// Always live (no cache).  Acquires a slot bound to the server's
/// `default_db` for the one query.
#[tauri::command]
pub async fn list_databases(
    server_id: i64,
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<Vec<DatabaseInfo>, CommandError> {
    let conn = store::get(&pool, server_id)
        .await?
        .ok_or_else(|| CommandError::unknown_connection(server_id))?;

    let handle = registry
        .by_id
        .get(&server_id)
        .ok_or_else(|| CommandError::not_connected(server_id))?;
    let slot_manager = handle.slot_manager.clone();
    drop(handle);

    let guard = slot_manager
        .acquire(&conn.default_db)
        .await
        .map_err(|e: SlotError| CommandError::Slot(e.to_string()))?;

    Ok(introspect::list_databases(&guard).await?)
}

/// List schemas in `database` for `server_id`.  Session-cache-backed; on
/// miss, fully introspects the database and populates the in-memory cache.
#[tauri::command]
pub async fn list_schemas(
    server_id: i64,
    database: String,
    registry: State<'_, ServerRegistry>,
) -> Result<Vec<String>, CommandError> {
    let payload = ensure_payload(server_id, &database, &registry).await?;
    Ok(payload.schemas.into_iter().map(|s| s.name).collect())
}

/// List tables / views / materialized views / partitioned tables in
/// `schema` of `database` for `server_id`.  Session-cache-backed; returns
/// an empty vec if the schema isn't present in the payload.
#[tauri::command]
pub async fn list_relations(
    server_id: i64,
    database: String,
    schema: String,
    registry: State<'_, ServerRegistry>,
) -> Result<Vec<RelationInfo>, CommandError> {
    let payload = ensure_payload(server_id, &database, &registry).await?;
    Ok(payload
        .schemas
        .into_iter()
        .find(|s| s.name == schema)
        .map(|s| s.relations)
        .unwrap_or_default())
}

/// List functions / procedures / aggregates / windows in `schema` of
/// `database` for `server_id`.  Session-cache-backed.
#[tauri::command]
pub async fn list_functions(
    server_id: i64,
    database: String,
    schema: String,
    registry: State<'_, ServerRegistry>,
) -> Result<Vec<FunctionInfo>, CommandError> {
    let payload = ensure_payload(server_id, &database, &registry).await?;
    Ok(payload
        .schemas
        .into_iter()
        .find(|s| s.name == schema)
        .map(|s| s.functions)
        .unwrap_or_default())
}

/// Return the full schema payload for `(server_id, database)`.
///
/// On cache miss this acquires a slot and runs a full introspection (same
/// path as `list_schemas`).  On hit it returns the cached payload at zero
/// slot cost.  The frontend's `schemaStore` is the primary consumer.
#[tauri::command]
pub async fn get_schema_payload(
    server_id: i64,
    database: String,
    registry: State<'_, ServerRegistry>,
) -> Result<SchemaPayload, CommandError> {
    ensure_payload(server_id, &database, &registry).await
}

/// Evict `database` from the session schema cache for `server_id`.
///
/// The frontend calls `clearDatabaseSubtree` immediately after, which sets
/// `children = null` on the database tree node so the next expand triggers
/// a fresh introspection via `ensure_payload`.
#[tauri::command]
pub async fn refresh_schema_cache(
    server_id: i64,
    database: String,
    registry: State<'_, ServerRegistry>,
) -> Result<(), CommandError> {
    let handle = registry
        .by_id
        .get(&server_id)
        .ok_or_else(|| CommandError::not_connected(server_id))?;
    handle.schema_cache.remove(&database);
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Completion analysis
// ═══════════════════════════════════════════════════════════════════════════

use crate::parse::{self, CompletionContext};

/// Analyze the SQL buffer at the given UTF-8 byte offset.
///
/// Pure, sync; Tauri runs sync handlers on a blocking pool.  No Postgres
/// connection is acquired.
#[tauri::command]
pub fn analyze_completion(sql: String, cursor: usize) -> CompletionContext {
    parse::analyze_completion(&sql, cursor)
}

// ═══════════════════════════════════════════════════════════════════════════
// History
// ═══════════════════════════════════════════════════════════════════════════

use crate::history::{self, HistoryFilter, HistoryRecord, NewHistoryRecord};

#[tauri::command]
pub async fn list_history(
    limit: Option<i64>,
    server_id: Option<i64>,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<Vec<HistoryRecord>, CommandError> {
    let lim = limit.unwrap_or(history::HISTORY_RETENTION as i64);
    Ok(history::list(&pool, lim, HistoryFilter { server_id }).await?)
}

#[tauri::command]
pub async fn clear_history(pool: State<'_, sqlx::SqlitePool>) -> Result<(), CommandError> {
    history::clear(&pool).await?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Saved queries
// ═══════════════════════════════════════════════════════════════════════════

use crate::saved::{self, NewSavedQuery, SavedQuery};

#[tauri::command]
pub async fn list_saved(
    server_id: Option<i64>,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<Vec<SavedQuery>, CommandError> {
    Ok(saved::list(&pool, server_id).await?)
}

#[tauri::command]
pub async fn save_query(
    new: NewSavedQuery,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<SavedQuery, CommandError> {
    Ok(saved::create(&pool, new).await?)
}

#[tauri::command]
pub async fn delete_saved(id: i64, pool: State<'_, sqlx::SqlitePool>) -> Result<(), CommandError> {
    saved::delete(&pool, id).await?;
    Ok(())
}

#[tauri::command]
pub async fn rename_saved(
    id: i64,
    new_name: String,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<SavedQuery, CommandError> {
    Ok(saved::rename(&pool, id, &new_name).await?)
}

// ═══════════════════════════════════════════════════════════════════════════
// Cancellation
// ═══════════════════════════════════════════════════════════════════════════

/// Outcome of a `cancel_query` invocation.  `cancelled` counts how many
/// CancelRequest packets we successfully dispatched; `errors` collects any
/// per-handle failures.  v1 surfaces `errors` for debugging but the
/// frontend can ignore them.
#[derive(Debug, Serialize)]
pub struct CancelOutcome {
    pub cancelled: usize,
    pub errors: Vec<String>,
}

/// Cancel every in-flight query on `server_id`, optionally filtered to
/// queries running against `database`.
///
/// Does **not** acquire a slot.  Each cancel opens a fresh TCP connection
/// to the server, sends the Postgres CancelRequest packet, and closes —
/// AGENTS.md principle 1 still holds because the cancel is a *direct
/// consequence of an explicit user action* (the Cancel button).
#[tauri::command]
pub async fn cancel_query(
    server_id: i64,
    database: Option<String>,
    registry: State<'_, ServerRegistry>,
) -> Result<CancelOutcome, CommandError> {
    let handle = registry
        .by_id
        .get(&server_id)
        .ok_or_else(|| CommandError::not_connected(server_id))?;

    let slot_manager = handle.slot_manager.clone();
    drop(handle); // release DashMap shard lock before awaiting

    let handles = slot_manager.busy_cancel_handles(database.as_deref());

    // Empty fan-out is a no-op success — easier for the frontend than an error.
    if handles.is_empty() {
        return Ok(CancelOutcome {
            cancelled: 0,
            errors: Vec::new(),
        });
    }

    // Run every cancel concurrently.
    let mut tasks = tokio::task::JoinSet::new();
    for h in handles {
        tasks.spawn(async move { h.cancel().await });
    }

    let mut cancelled = 0usize;
    let mut errors: Vec<String> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(())) => cancelled += 1,
            Ok(Err(msg)) => errors.push(msg),
            Err(join_err) => errors.push(format!("task panicked: {join_err}")),
        }
    }

    Ok(CancelOutcome { cancelled, errors })
}

// ═══════════════════════════════════════════════════════════════════════════
// Unit tests
// ═══════════════════════════════════════════════════════════════════════════

// (bare-SELECT tests moved to query/mod.rs)
