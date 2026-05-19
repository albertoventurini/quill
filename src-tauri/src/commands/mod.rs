//! Tauri command surface — the IPC bridge between frontend and backend.
//!
//! Every command that needs a database connection goes through the
//! `ServerRegistry` (which holds per-server `SlotManager`s).  No command
//! opens a Postgres connection eagerly — connections happen only inside
//! `run_query` (AGENTS.md principle 1).

use std::time::Instant;

use secrecy::SecretString;
use serde::Serialize;
use serde_json::Value;
use sqlx::Column;
use sqlx::Row;
use sqlx::TypeInfo;
use sqlx::postgres::PgRow;
use tauri::State;

use crate::pg::PgConnector;
use crate::registry::{ServerHandle, ServerRegistry};
use crate::slots::{SlotError, SlotState};
use crate::store;

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
    UnknownDatabase(String),
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
            | Self::UnknownDatabase(msg) => write!(f, "{msg}"),
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
    #[allow(dead_code)]
    fn unknown_database(server_id: i64, database: &str) -> Self {
        Self::UnknownDatabase(format!(
            "database '{database}' is not cached for server {server_id}; expand it in the tree or call refresh_schema_cache"
        ))
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

// ═══════════════════════════════════════════════════════════════════════════
// Query result types
// ═══════════════════════════════════════════════════════════════════════════

/// Returned by `run_query`.  The frontend renders `rows` as a table using
/// the `columns` metadata.
#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: usize,
    pub duration_ms: u64,
}

/// Metadata for one result-set column.
#[derive(Debug, Serialize)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
}

// ═══════════════════════════════════════════════════════════════════════════
// Row → JSON conversion
// ═══════════════════════════════════════════════════════════════════════════

/// Convert a single `PgRow` into a `Vec<serde_json::Value>`.
///
/// Switches on the column's Postgres type name.  Unrecognised types fall
/// back to a best-effort `String` representation — this function **never
/// errors**; the user always sees something.
pub fn pg_row_to_json(row: &PgRow) -> Vec<Value> {
    let columns = row.columns();
    let mut values = Vec::with_capacity(columns.len());

    for (i, col) in columns.iter().enumerate() {
        let type_name = col.type_info().name();
        let val = match type_name {
            "BOOL" => row
                .try_get::<bool, _>(i)
                .map(Value::Bool)
                .unwrap_or(Value::Null),

            "INT2" => row
                .try_get::<i16, _>(i)
                .map(|v| Value::Number((v as i64).into()))
                .unwrap_or(Value::Null),

            "INT4" => row
                .try_get::<i32, _>(i)
                .map(|v| Value::Number((v as i64).into()))
                .unwrap_or(Value::Null),

            "INT8" => row
                .try_get::<i64, _>(i)
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null),

            "FLOAT4" => row
                .try_get::<f32, _>(i)
                .map(|v| {
                    serde_json::Number::from_f64(v as f64)
                        .map(Value::Number)
                        .unwrap_or(Value::Null)
                })
                .unwrap_or(Value::Null),

            "FLOAT8" => row
                .try_get::<f64, _>(i)
                .map(|v| {
                    serde_json::Number::from_f64(v)
                        .map(Value::Number)
                        .unwrap_or(Value::Null)
                })
                .unwrap_or(Value::Null),

            "JSON" | "JSONB" => row.try_get::<Value, _>(i).unwrap_or(Value::Null),

            "BYTEA" => row
                .try_get::<Vec<u8>, _>(i)
                .map(|bytes| {
                    use base64::Engine;
                    Value::String(base64::engine::general_purpose::STANDARD.encode(&bytes))
                })
                .unwrap_or(Value::Null),

            "TEXT" | "VARCHAR" | "BPCHAR" | "CHAR" | "NAME" | "UUID" | "DATE" | "TIME"
            | "TIMESTAMP" | "TIMESTAMPTZ" | "NUMERIC" | "OID" => row
                .try_get::<String, _>(i)
                .map(Value::String)
                .unwrap_or(Value::Null),

            // any unrecognised type — best-effort String fallback
            _ => row
                .try_get::<String, _>(i)
                .map(Value::String)
                .unwrap_or(Value::Null),
        };
        values.push(val);
    }

    values
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
) -> Result<(), CommandError> {
    // Remove and take the handle out of the map so disconnect_all runs
    // outside the DashMap shard lock.
    let handle = registry
        .by_id
        .remove(&id)
        .map(|(_, h)| h)
        .ok_or_else(|| CommandError::not_connected(id))?;

    handle.slot_manager.disconnect_all().await;
    Ok(())
}

/// Run a SQL query against a connected server.
///
/// 1. Look up the `ServerHandle` in the registry; return `NotConnected` if absent.
/// 2. Acquire a slot bound to `database` via `slot_manager.acquire()` — this
///    is the **only** place where a Postgres connection may be opened.
/// 3. Execute the SQL; time it.
/// 4. Convert rows to JSON; return a `QueryResult`.
#[tauri::command]
pub async fn run_query(
    server_id: i64,
    database: String,
    sql: String,
    registry: State<'_, ServerRegistry>,
) -> Result<QueryResult, CommandError> {
    let handle = registry
        .by_id
        .get(&server_id)
        .ok_or_else(|| CommandError::not_connected(server_id))?;

    // Clone the Arc out of the map so we don't hold a DashMap shard lock
    // across the `.await`.
    let slot_manager = handle.slot_manager.clone();
    drop(handle);

    let mut guard = slot_manager
        .acquire(&database)
        .await
        .map_err(|e: SlotError| CommandError::Slot(e.to_string()))?;

    // Reject bare SELECT — Postgres treats it as a single empty row,
    // but the user meant to type a real query.
    {
        let bare = sql.trim().trim_matches(';').trim();
        if bare.eq_ignore_ascii_case("SELECT") {
            return Err(CommandError::Pg(
                "incomplete query: SELECT requires a column list".into(),
            ));
        }
    }

    let start = Instant::now();
    let rows: Vec<PgRow> = sqlx::query(&sql)
        .fetch_all(&mut *guard)
        .await
        .map_err(|e| CommandError::Pg(e.to_string()))?;
    let duration_ms = start.elapsed().as_millis() as u64;

    // Extract column metadata from the first row (if any).
    let columns: Vec<ColumnMeta> = rows
        .first()
        .map(|r| {
            r.columns()
                .iter()
                .map(|col| ColumnMeta {
                    name: col.name().to_string(),
                    type_name: col.type_info().name().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    let row_count = rows.len();
    let json_rows: Vec<Vec<Value>> = rows.iter().map(pg_row_to_json).collect();

    // guard dropped here — slot returns to idle.

    Ok(QueryResult {
        columns,
        rows: json_rows,
        row_count,
        duration_ms,
    })
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

/// Fetch the payload for `(server_id, database)` from the local cache,
/// or — on miss — acquire a slot, introspect the database, write the cache,
/// and return the freshly-built payload.
///
/// `ensure_payload` is the only path through which a schema-cache row is
/// born on an implicit expand; the explicit `refresh_schema_cache` command
/// bypasses the cache check and always re-introspects.
async fn ensure_payload(
    server_id: i64,
    database: &str,
    pool: &sqlx::SqlitePool,
    registry: &ServerRegistry,
) -> Result<SchemaPayload, CommandError> {
    if let Some(row) = store::get_schema_cache(pool, server_id, database).await? {
        let payload: SchemaPayload = serde_json::from_str(&row.payload_json)
            .map_err(|e| CommandError::Introspect(format!("cached payload is unreadable: {e}")))?;
        return Ok(payload);
    }

    // Cache miss — introspect.
    let payload = run_introspection(server_id, database, registry).await?;

    let json = serde_json::to_string(&payload)
        .map_err(|e| CommandError::Introspect(format!("payload serialize failed: {e}")))?;
    store::set_schema_cache(pool, server_id, database, &json).await?;

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

    let mut guard = slot_manager
        .acquire(database)
        .await
        .map_err(|e: SlotError| CommandError::Slot(e.to_string()))?;

    Ok(introspect::introspect_database(&mut guard).await?)
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

    let mut guard = slot_manager
        .acquire(&conn.default_db)
        .await
        .map_err(|e: SlotError| CommandError::Slot(e.to_string()))?;

    Ok(introspect::list_databases(&mut guard).await?)
}

/// List schemas in `database` for `server_id`.  Cache-backed; on miss,
/// fully introspects the database and writes the cache.
#[tauri::command]
pub async fn list_schemas(
    server_id: i64,
    database: String,
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<Vec<String>, CommandError> {
    let payload = ensure_payload(server_id, &database, &pool, &registry).await?;
    Ok(payload.schemas.into_iter().map(|s| s.name).collect())
}

/// List tables / views / materialized views / partitioned tables in
/// `schema` of `database` for `server_id`.  Cache-backed; returns an empty
/// vec if the schema isn't present in the cached payload.
#[tauri::command]
pub async fn list_relations(
    server_id: i64,
    database: String,
    schema: String,
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<Vec<RelationInfo>, CommandError> {
    let payload = ensure_payload(server_id, &database, &pool, &registry).await?;
    Ok(payload
        .schemas
        .into_iter()
        .find(|s| s.name == schema)
        .map(|s| s.relations)
        .unwrap_or_default())
}

/// List functions / procedures / aggregates / windows in `schema` of
/// `database` for `server_id`.  Cache-backed.
#[tauri::command]
pub async fn list_functions(
    server_id: i64,
    database: String,
    schema: String,
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<Vec<FunctionInfo>, CommandError> {
    let payload = ensure_payload(server_id, &database, &pool, &registry).await?;
    Ok(payload
        .schemas
        .into_iter()
        .find(|s| s.name == schema)
        .map(|s| s.functions)
        .unwrap_or_default())
}

/// Force a fresh introspection of `database` on `server_id`, overwriting
/// the cache row.  Returns the newly-cached payload so the caller can
/// re-render without a second round-trip.
#[tauri::command]
pub async fn refresh_schema_cache(
    server_id: i64,
    database: String,
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<SchemaPayload, CommandError> {
    let payload = run_introspection(server_id, &database, &registry).await?;
    let json = serde_json::to_string(&payload)
        .map_err(|e| CommandError::Introspect(format!("payload serialize failed: {e}")))?;
    store::set_schema_cache(&pool, server_id, &database, &json).await?;
    Ok(payload)
}

// ═══════════════════════════════════════════════════════════════════════════
// Unit tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    /// Test the bare-SELECT detection used by `run_query`.
    fn is_bare_select(sql: &str) -> bool {
        let bare = sql.trim().trim_matches(';').trim();
        bare.eq_ignore_ascii_case("SELECT")
    }

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
