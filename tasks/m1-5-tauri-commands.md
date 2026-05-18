# M1.5 — Tauri command surface + hardcoded `SELECT 1` smoke test

## Goal

**Before:** The Rust backend has fully working modules — `store` (SQLite CRUD for connections), `slots` (budgeted connection-pool replacement), `pg` (real Postgres connector), and `registry` (per-process map of live server handles). But the frontend cannot reach any of them: the only Tauri command is the scaffold `greet`, the `invoke_handler` only registers `greet`, and no capability entry allows IPC calls. The sole integration test (`pg_integration.rs`) exercises `PgConnector` and `SlotManager` directly without routing through any command.

**After:** Seven typed Tauri commands expose the full connection lifecycle to the frontend — list/save/delete stored connections, connect/disconnect a server (inserting/removing a `ServerHandle` in the registry), run a SQL query through the slot manager and get a typed `QueryResult`, and poll slot state. `connect_server` never eagerly opens a Postgres connection (AGENTS.md principle 1). A smoke test (`smoke_select_1.rs`) exercises the full path end-to-end — store → connect → run `SELECT 1` → verify slot state → disconnect — by calling the command functions directly (not via Tauri IPC), gated on `QUILL_TEST_PG_URL`. The command surface is now usable from the Svelte frontend in M1.6.

## Current state

Every file listed below **already exists** and will be modified or created. Read them in full before writing anything.

### `src-tauri/Cargo.toml`

```toml
[package]
name = "quill"
version = "0.1.0"
description = "A Tauri App"
authors = ["you"]
edition = "2024"

# See more keys and their definitions at https://doc.rust-lang.org/cargo/reference/manifest.html

[lib]
# The `_lib` suffix may seem redundant but it is necessary
# to make the lib name unique and wouldn't conflict with the bin name.
# This seems to be only an issue on Windows, see https://github.com/rust-lang/cargo/issues/8519
name = "quill_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-opener = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sqlx = { version = "0.8", features = ["postgres", "sqlite", "runtime-tokio", "macros", "migrate"] }
async-trait = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }
thiserror = "2"
secrecy = "0.10"
dashmap = "6"

[dev-dependencies]
url = "2"
```

### `src-tauri/src/lib.rs`

```rust
pub mod pg;
pub mod registry;
pub mod slots;
pub mod store;

use tauri::Manager;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle();
            let pool = tauri::async_runtime::block_on(store::open(handle))?;
            app.manage(pool);
            app.manage(registry::ServerRegistry::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

### `src-tauri/capabilities/default.json`

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Capability for the main window",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "opener:default"
  ]
}
```

### `src-tauri/src/slots/mod.rs`

Full file is 843 lines. The existing `Deref` + `DerefMut` + `AsRef` impls on `SlotGuard` are:

```rust
impl<C: Connector> Deref for SlotGuard<'_, C> {
    type Target = C::Conn;
    fn deref(&self) -> &C::Conn {
        self.conn.as_ref().expect("SlotGuard always holds a connection")
    }
}

impl<C: Connector> std::ops::DerefMut for SlotGuard<'_, C> {
    fn deref_mut(&mut self) -> &mut C::Conn {
        self.conn.as_mut().expect("SlotGuard always holds a connection")
    }
}

impl<C: Connector> AsRef<C::Conn> for SlotGuard<'_, C> {
    fn as_ref(&self) -> &C::Conn {
        self.conn.as_ref().expect("SlotGuard always holds a connection")
    }
}
```

Public types relevant to M1.5: `SlotManager`, `SlotGuard`, `SlotState`, `SlotInfo`, `SlotError`, `Connector`, `ConnectorError`. All are `pub` from `crate::slots` (the module is `pub mod` in `lib.rs`).

### `src-tauri/src/store/mod.rs`

Full file is 266 lines. The module starts with `#![allow(dead_code)]` — commands will consume its functions, so this stays.

Public types: `Connection`, `NewConnection`, `StoreError`.
Public free functions: `open`, `list`, `get`, `insert`, `delete`.

The `Connection` struct fields (used in `connect_server` to build a `PgConnector`):
```rust
pub struct Connection {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: i32,          // NOTE: PgConnector uses u16
    pub default_db: String,
    pub username: String,
    pub ssl_mode: String,   // NOTE: PgConnector uses PgSslMode (parse via parse_ssl_mode)
    pub slot_budget: i32,   // NOTE: SlotManager uses usize
    pub password_ref: Option<String>,
    pub created_at: String,
}
```

### `src-tauri/src/pg/mod.rs`

Full file is 69 lines. Public type: `PgConnector` with fields `host`, `port`, `username`, `password`, `ssl_mode`. Public constructor `parse_ssl_mode`. Implements `crate::slots::Connector`.

### `src-tauri/src/registry.rs`

Full file is 36 lines. Public types: `ServerHandle`, `ServerRegistry`. `ServerHandle` wraps `Arc<SlotManager<PgConnector>>`. `ServerRegistry` wraps `DashMap<i64, ServerHandle>`.

### `src-tauri/src/main.rs`

```rust
// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    quill_lib::run()
}
```

Not modified by this task — listed for awareness.

### `src-tauri/tests/pg_integration.rs`

Already exists (147 lines). Not modified by this task — listed so the implementer knows the test directory is already populated and the `$QUILL_TEST_PG_URL` gating pattern must be reused.

## Deliverables

### 1. `src-tauri/Cargo.toml` — add `base64` dependency

Add one line below `dashmap = "6"`:

```toml
base64 = "0.22"
```

Full `[dependencies]` section after the change:

```toml
[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-opener = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sqlx = { version = "0.8", features = ["postgres", "sqlite", "runtime-tokio", "macros", "migrate"] }
async-trait = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }
thiserror = "2"
secrecy = "0.10"
dashmap = "6"
base64 = "0.22"
```

`base64` 0.22 is the crate needed for bytea-to-base64 encoding in the row conversion helper. Its API:

```rust
use base64::Engine;
base64::engine::general_purpose::STANDARD.encode(&bytes)
```

### 2. `src-tauri/src/commands/mod.rs` — new file

Create the file at `src-tauri/src/commands/mod.rs`. This is the primary deliverable. It contains:

1. `CommandError` enum
2. `QueryResult` and `ColumnMeta` structs
3. `pg_row_to_json` helper
4. Seven `#[tauri::command]` functions

Full content:

```rust
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
use sqlx::postgres::PgRow;
use sqlx::Row;
use tauri::State;

use crate::pg::PgConnector;
use crate::registry::{ServerHandle, ServerRegistry};
use crate::slots::SlotManager;
use crate::store;
use crate::slots::{SlotState, SlotError};

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
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownConnection(msg) => write!(f, "{msg}"),
            Self::NotConnected(msg) => write!(f, "{msg}"),
            Self::Slot(msg) => write!(f, "{msg}"),
            Self::Pg(msg) => write!(f, "{msg}"),
            Self::Store(msg) => write!(f, "{msg}"),
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
fn pg_row_to_json(row: &PgRow) -> Vec<Value> {
    let columns = row.columns();
    let mut values = Vec::with_capacity(columns.len());

    for (i, col) in columns.iter().enumerate() {
        let type_name = col.type_info().name();
        let val = match type_name {
            "bool" => row
                .try_get::<bool, _>(i)
                .map(Value::Bool)
                .unwrap_or(Value::Null),

            "int2" => row
                .try_get::<i16, _>(i)
                .map(|v| Value::Number((v as i64).into()))
                .unwrap_or(Value::Null),

            "int4" => row
                .try_get::<i32, _>(i)
                .map(|v| Value::Number((v as i64).into()))
                .unwrap_or(Value::Null),

            "int8" => row
                .try_get::<i64, _>(i)
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null),

            "float4" => row
                .try_get::<f32, _>(i)
                .and_then(|v| serde_json::Number::from_f64(v as f64).map(Value::Number))
                .unwrap_or(Value::Null),

            "float8" => row
                .try_get::<f64, _>(i)
                .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
                .unwrap_or(Value::Null),

            "json" | "jsonb" => row
                .try_get::<Value, _>(i)
                .unwrap_or(Value::Null),

            "bytea" => row
                .try_get::<Vec<u8>, _>(i)
                .map(|bytes| {
                    use base64::Engine;
                    Value::String(base64::engine::general_purpose::STANDARD.encode(&bytes))
                })
                .unwrap_or(Value::Null),

            // text, varchar, name, date, time, timestamp, timestamptz,
            // uuid, and anything else — fall back to String.
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
    let ssl_mode = PgConnector::parse_ssl_mode(&conn.ssl_mode)
        .map_err(|e| CommandError::Pg(e.0))?;
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

    // Clone the Arc out of the map so we don't hold a DashMap shar'd lock
    // across the `.await`.
    let slot_manager = handle.slot_manager.clone();
    drop(handle);

    let mut guard = slot_manager
        .acquire(&database)
        .await
        .map_err(|e: SlotError| CommandError::Slot(e.to_string()))?;

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
    let json_rows: Vec<Vec<Value>> = rows.iter().map(|row| pg_row_to_json(row)).collect();

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
    Ok(registry.by_id.get(&server_id).map(|h| h.slot_manager.state()))
}
```

### 3. `src-tauri/src/lib.rs` — register the commands module and commands

Replace the file entirely. Three changes from the current file:

- Add `pub mod commands;` alongside the existing module declarations.
- Remove the `greet` command.
- Replace `invoke_handler` with all seven new commands.

```rust
pub mod commands;
pub mod pg;
pub mod registry;
pub mod slots;
pub mod store;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle();
            let pool = tauri::async_runtime::block_on(store::open(handle))?;
            app.manage(pool);
            app.manage(registry::ServerRegistry::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_connections,
            commands::save_connection,
            commands::delete_connection,
            commands::connect_server,
            commands::disconnect_server,
            commands::run_query,
            commands::get_slot_state,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

### 4. `src-tauri/capabilities/default.json` — permit the new commands

Replace the file. Add `"quill:default"` to the permissions array — this grants the webview access to every command in the `quill` crate.

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Capability for the main window",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "opener:default",
    "quill:default"
  ]
}
```

### 5. `src-tauri/tests/smoke_select_1.rs` — new file

Create the file. Gated on `QUILL_TEST_PG_URL`; follows the same `skip_note()` pattern as `pg_integration.rs`.

```rust
//! Smoke test: full end-to-end flow through the command code paths.
//!
//! Exercises every layer below the `#[tauri::command]` wrappers — store,
//! registry, slot manager, and query execution — in the same order the
//! real commands would.  Can't call the `#[tauri::command]` functions
//! directly because they require `tauri::State` (only available inside
//! a Tauri runtime), so we replicate the logic inline.
//!
//! Run with:
//!   QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh
//!
//! Without the env var, every test silently passes after a stderr note.

use sqlx::postgres::PgRow;
use sqlx::Row;

use secrecy::SecretString;

use quill_lib::pg::PgConnector;
use quill_lib::registry::{ServerHandle, ServerRegistry};
use quill_lib::store;

/// Parsed test-DSN pieces.
struct TestDsn {
    host: String,
    port: u16,
    username: String,
    password: String,
    database: String,
}

fn dsn() -> Option<TestDsn> {
    let raw = std::env::var("QUILL_TEST_PG_URL").ok()?;
    let u = url::Url::parse(&raw).expect("QUILL_TEST_PG_URL must be a valid postgres URL");
    Some(TestDsn {
        host: u.host_str()?.to_string(),
        port: u.port().unwrap_or(5432),
        username: u.username().to_string(),
        password: u.password().unwrap_or("").to_string(),
        database: u.path().trim_start_matches('/').to_string(),
    })
}

fn skip_note() {
    eprintln!("QUILL_TEST_PG_URL not set; skipping smoke_select_1 test");
}

// ── Test 1: full cycle (store → connect → query → state → disconnect) ──

#[tokio::test]
async fn full_cycle_store_to_disconnect() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    // ── 1. In-memory SQLite store with migrations ────────────────────
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrations");

    // ── 2. Insert a connection row ───────────────────────────────────
    let conn = store::insert(
        &pool,
        store::NewConnection {
            name: "smoke-server".into(),
            host: dsn.host.clone(),
            port: dsn.port as i32,
            default_db: dsn.database.clone(),
            username: dsn.username.clone(),
            ssl_mode: "disable".into(),
            slot_budget: 2,
            password_ref: None,
        },
    )
    .await
    .expect("insert");
    let server_id = conn.id;

    // ── 3. Simulate `connect_server` — build connector, create registry entry ──
    let registry = ServerRegistry::default();

    // Re-read from store (like the real connect_server would).
    let conn = store::get(&pool, server_id)
        .await
        .expect("get")
        .expect("row exists");

    let ssl_mode =
        PgConnector::parse_ssl_mode(&conn.ssl_mode).expect("ssl_mode");
    let connector = PgConnector {
        host: conn.host.clone(),
        port: conn.port as u16,
        username: conn.username.clone(),
        password: SecretString::from(dsn.password.clone()),
        ssl_mode,
    };

    let budget = conn.slot_budget.max(1) as usize;
    let handle = ServerHandle::new(connector, budget);

    // Assert: connect_server must NOT eagerly open connections.
    let state = handle.slot_manager.state();
    assert_eq!(state.budget, 2);
    assert_eq!(state.slots.len(), 2);
    assert!(
        state.slots.iter().all(|s| !s.busy),
        "connect_server must not open connections eagerly"
    );
    assert!(
        state.slots.iter().all(|s| s.database.is_empty()),
        "no slots should be bound after connect_server"
    );

    registry.by_id.insert(server_id, handle);

    // ── 4. Simulate `get_slot_state` ─────────────────────────────────
    {
        let h = registry.by_id.get(&server_id).expect("handle");
        let state = h.slot_manager.state();
        assert_eq!(state.budget, 2);
        assert_eq!(state.slots.len(), 2);
    }

    // ── 5. Simulate `run_query` — SELECT 1 AS one ────────────────────
    let (row_count, col_name, post_state) = {
        let h = registry.by_id.get(&server_id).expect("handle");
        let slot_manager = h.slot_manager.clone();
        drop(h);

        let mut guard = slot_manager
            .acquire(&dsn.database)
            .await
            .expect("acquire");

        let rows: Vec<PgRow> = sqlx::query("SELECT 1 AS one")
            .fetch_all(&mut *guard)
            .await
            .expect("SELECT 1");

        let col_name = rows[0].columns()[0].name().to_string();
        let val: i32 = rows[0].try_get(0).expect("column 0 as i32");
        assert_eq!(val, 1);

        drop(guard); // slot returns to idle

        let state = slot_manager.state();
        (rows.len(), col_name, state)
    };

    assert_eq!(row_count, 1);
    assert_eq!(col_name, "one", "column alias should be preserved");

    // After guard drop — one idle slot bound to the test database.
    let bound: Vec<_> = post_state
        .slots
        .iter()
        .filter(|s| !s.database.is_empty())
        .collect();
    assert_eq!(bound.len(), 1, "one slot should be bound after query");
    assert_eq!(bound[0].database, dsn.database);
    assert!(!bound[0].busy, "bound slot should be idle after guard drop");

    // ── 6. Simulate `disconnect_server` — remove from registry, close all ──
    {
        let handle = registry
            .by_id
            .remove(&server_id)
            .map(|(_, h)| h)
            .expect("handle exists");

        handle.slot_manager.disconnect_all().await;
    }

    assert!(
        registry.by_id.is_empty(),
        "registry should be empty after disconnect"
    );

    // ── 7. Clean up the store row ────────────────────────────────────
    store::delete(&pool, server_id).await.expect("delete");
}

// ── Test 2: `get_slot_state` returns None for unknown server ──────────

#[tokio::test]
async fn slot_state_none_for_unknown_server() {
    let registry = ServerRegistry::default();

    assert!(
        registry.by_id.get(&999).is_none(),
        "unregistered server should return None"
    );
}

// ── Test 3: CommandError serialization shape ──────────────────────────

#[test]
fn command_error_serde_shape() {
    let err = quill_lib::commands::CommandError::Pg("auth failed".into());
    let json = serde_json::to_value(err).expect("serialize");

    assert_eq!(json["kind"], "Pg");
    assert_eq!(json["message"], "auth failed");

    let err = quill_lib::commands::CommandError::UnknownConnection(
        "connection 42 not found".into(),
    );
    let json = serde_json::to_value(err).expect("serialize");
    assert_eq!(json["kind"], "UnknownConnection");
    assert_eq!(json["message"], "connection 42 not found");
}
```

## Implementation order

Touch files in this order. There are **no intermediate compile errors** if you follow this sequence.

1. **`src-tauri/Cargo.toml`** — add `base64 = "0.22"`. Verify: `( cd src-tauri && cargo build )` — the existing code still compiles, the new dep is just downloaded.

2. **`src-tauri/src/commands/mod.rs`** — write the new file. It will not yet compile because `lib.rs` hasn't declared the module. Add the file, then stop — do **not** try to build yet.

3. **`src-tauri/src/lib.rs`** — add `pub mod commands;`, remove `greet`, replace `invoke_handler`. This is the first point the commands module is type-checked. Verify: `( cd src-tauri && cargo build )` — must succeed.

4. **`src-tauri/capabilities/default.json`** — add `"quill:default"`. Verify: `( cd src-tauri && cargo build )` — no compile impact, but ensures the Tauri builder can resolve permissions at runtime.

5. **`src-tauri/tests/smoke_select_1.rs`** — write the integration test. Run `./test.sh` to confirm unit tests still pass and the smoke tests cleanly skip without `QUILL_TEST_PG_URL`. Then start a Docker Postgres and re-run with the env var set.

## Known gotchas

- **`State<'_, SqlitePool>` vs imported type.** In Tauri 2, `State` is `tauri::State`. The commands file imports `use tauri::State;` — do not import it from `tauri::Manager`. `State` requires the managed type to be `Send + Sync + 'static`; `sqlx::SqlitePool` and `ServerRegistry` both satisfy this.

- **`CommandError` must implement `Serialize`, `Display`, and `std::error::Error`.** Tauri 2 requires `Error` for the error return type. Our manual `impl std::error::Error for CommandError {}` satisfies this. The `Serialize` impl (via `#[derive(Serialize)]` + `#[serde(tag = "kind", content = "message")]`) is what the frontend receives. Missing either impl produces opaque Tauri errors.

- **`#[serde(tag = "kind", content = "message")]` on a unit-variant-free enum.** The derive macro on `CommandError` works because every variant has exactly one field — serde maps the variant name to `kind` and the inner value to `message`. If any variant were unit-like (`Store` with no field), serialization would fail silently (Tauri would return `{}`). All variants must carry a `String`.

- **`SlotState` vs `CommandError::Slot` naming collision.** The file imports `use crate::slots::SlotState;` (the type from the slot manager). `CommandError::Slot` is a variant name, not a type — no conflict. Just don't name anything else `SlotState` in `commands/mod.rs`.

- **`store::Connection` port is `i32`; `PgConnector` port is `u16`.** Use `conn.port as u16` when constructing the connector. Postgres ports are always ≤ 65535 and never negative in practice, so the cast is safe. A future migration could change the SQLite column to `INTEGER CHECK (port > 0 AND port <= 65535)` but that is not M1.5's job.

- **`store::Connection.slot_budget` is `i32`; `SlotManager` budget is `usize`.** Use `conn.slot_budget.max(1) as usize`. On 64-bit systems `usize` is always large enough. The `.max(1)` guard prevents a zero-budget slot manager (which would make all `acquire` calls return `AllBusy(0)`).

- **`DashMap::get` returns `Ref<'_, K, V>` — do not hold it across `.await`.** `run_query` clones the `Arc<SlotManager>` out of the map and then drops the `Ref`. This is why the code uses `handle.slot_manager.clone()` followed by `drop(handle)`. Holding a `DashMap` shard lock across an `.await` can deadlock if another task tries to access the same shard.

- **`SqlitePool` in `State` and `sqlx::SqlitePool` type.** The `list_connections` command uses `State<'_, sqlx::SqlitePool>`. The `s` `ave_connection` and `delete_connection` commands do the same. The `open` function in `store` returns `sqlx::SqlitePool`, which is managed during `setup`. The type must match exactly — `sqlx::SqlitePool`, not `sqlx::Pool<sqlx::Sqlite>`.

- **`connect_server` calls `store::get` which may return `None`.** If the id doesn't exist, we return `CommandError::unknown_connection(id)`. This is tested implicitly by the smoke test (it never calls `connect_server` with a bad id — the test builds a registry directly), but the production path must handle it.

- **`run_query` column metadata extraction from first row.** If the query returns zero rows, `columns` is empty. The frontend must handle an empty `columns` array gracefully. No column info is available from `sqlx::query` without a result row in M1.

- **`serde_json::Number::from_f64` returns `Option<Number>`.** It returns `None` for `NaN` and `Infinity`. The `float4`/`float8` arms in `pg_row_to_json` use `.and_then(|v| Number::from_f64(...).map(Value::Number))` so non-finite floats become `Value::Null`. This is deliberate — JSON has no NaN.

- **`base64` 0.22 API.** The engine accessor is `base64::engine::general_purpose::STANDARD`. Older code uses `base64::encode()` which was removed in 0.20+.

- **`secrecy` 0.10 `SecretString::from(String)` works; `From<&str>` also works (via `From<&str> for SecretString`).** Use `SecretString::from(password)` where `password: String` — this moves the string, no clone.

- **Integration tests can only see `pub` items.** The `commands` module and its functions must be `pub`. All seven command functions are declared `pub`. The test file uses `quill_lib::commands` directly, so `commands` must be `pub mod` in `lib.rs`.

- **`./test.sh` runs `cargo test` from `src-tauri/`.** Files in `src-tauri/tests/` are picked up automatically. No test-runner config needed. Both `pg_integration.rs` and the new `smoke_select_1.rs` will compile and run together.

- **`quill:default` in capabilities may fail to resolve on first build after adding `pub mod commands;`.** Tauri 2 generates permission files during `tauri-build`. If the permission fails to resolve, `( cd src-tauri && cargo build )` once to regenerate the schema, then the runtime resolves correctly. If the `quill:default` permission isn't found, check that `tauri-build` completed (it's in `[build-dependencies]`).

- **`store::Connection` derives `Serialize` and `Deserialize`.** The `list_connections` and `save_connection` commands return `store::Connection` directly. This works because `store::Connection` already has `#[derive(Serialize, Deserialize)]` and Tauri 2 auto-serializes the return type. No wrapper needed.

- **`disconnect_server` uses `DashMap::remove`.** `remove(&key)` returns `Option<(K, V)>`. The `.map(|(_, h)| h)` extracts just the value. This is correct — the key is already known (it's the parameter `id`).

## Tests

The complete test file is in deliverable 5. Run them both ways:

```bash
# Without Postgres — every test silently passes after a stderr note.
./test.sh

# With Postgres — start the test container once, then run the full suite.
docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17
QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh
```

Coverage the file delivers:

1. **`full_cycle_store_to_disconnect`** — creates an in-memory SQLite pool with migrations; inserts a `connections` row via `store::insert`; reads it back (like `connect_server` would); builds a `PgConnector` + `ServerHandle` and inserts into a `ServerRegistry`; asserts no connection is opened eagerly (slots all idle and unbound); runs `SELECT 1 AS one` through `SlotManager::acquire` + `sqlx::query`; verifies the row is `[1]` and the column name is `"one"`; checks post-query slot state shows one idle bound slot; removes the handle from the registry and calls `disconnect_all`; asserts the registry is empty; cleans up the SQLite row. This is the closest possible test to the full `connect_server → run_query → get_slot_state → disconnect_server` cycle without a Tauri runtime.

2. **`slot_state_none_for_unknown_server`** — verifies that `get_slot_state` on a never-registered server id returns `None`.

3. **`command_error_serde_shape`** — unit test (no Postgres needed, no `QUILL_TEST_PG_URL` gating). Serializes `CommandError::Pg("auth failed".into())` and asserts the JSON is `{"kind":"Pg","message":"auth failed"}`. Also tests `CommandError::UnknownConnection`. This test runs every time `./test.sh` runs, even without Postgres.

No new unit tests inside `commands/mod.rs` itself. Unit-level testing of command functions without their managed state (pool, registry) has no value; tests 1 and 2 exercise the full path.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds on a machine **without** `QUILL_TEST_PG_URL` set — both `smoke_select_1` tests log "QUILL_TEST_PG_URL not set; skipping" to stderr and pass.
- [ ] `QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh` succeeds against a local `postgres:17` Docker container, with both smoke tests reporting `ok`. The existing `pg_integration` tests also still pass.
- [ ] `connect_server` does **not** open a Postgres connection (verify by reading the code: no call to `slot_manager.acquire()` or `PgConnection::connect_with` inside `connect_server`).
- [ ] `CommandError` serializes to `{"kind": "...", "message": "..."}`. Tested by `command_error_serde_shape` in `smoke_select_1.rs` (runs on every `./test.sh`, no PG needed).
- [ ] `grep -c "PgPool" src-tauri/src/commands/mod.rs` returns `0` (the slot manager is the pool).
- [ ] `src-tauri/src/lib.rs` declares `pub mod commands;` and the `invoke_handler` registers all seven commands (no `greet`).
- [ ] `src-tauri/capabilities/default.json` includes `"quill:default"` in the permissions array.
- [ ] Exactly two new files exist: `src-tauri/src/commands/mod.rs`, `src-tauri/tests/smoke_select_1.rs`. Modified files: `Cargo.toml`, `src/lib.rs`, `capabilities/default.json`. The files `store/mod.rs`, `slots/mod.rs`, `pg/mod.rs`, `registry.rs`, `main.rs`, `pg_integration.rs` are untouched.

## Out of scope

- UI consuming these commands — **M1.6**.
- Query cancellation — **M3**. The `CancelRequest` mechanism is entirely for M3; M1.5 just runs queries and lets the slot drop close normally.
- Streamed/paginated results — **M3**. `run_query` fetches all rows at once via `fetch_all`.
- Schema introspection commands (`list_databases`, etc.) — **M2**.
- OS keychain for passwords — **M6**. `password_ref` stays `NULL`; the password reaches `connect_server` as a plain `String` argument from the user's input in the frontend.
- `connect_server` writing anything back to the store — it only reads the `Connection` row. The password is never persisted.
- Any frontend code or TypeScript bridge (`src/lib/tauri.ts`) — **M1.6**.
