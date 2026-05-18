# M2.3 — Tauri commands for introspection (cache-first) + smoke test

## Goal

**Before:** The `introspect` module (M2.2) can pull a `SchemaPayload` from any open `PgConnection`, and the `store` module (M2.1) can persist that payload as JSON in `schema_cache`. Neither piece is reachable from the frontend — no Tauri commands wire them together. The slot manager has no way to be told "acquire a slot bound to *this* database for one introspection query, then release." If M2.4 tried to render a tree today, the only available command set is M1.5's `list_connections`/`run_query`/etc., which has nothing about databases, schemas, relations, or functions.

**After:** Five new Tauri commands form the introspection surface:

- `list_databases(server_id) -> Vec<DatabaseInfo>` — always live; acquires a slot bound to the server's `default_db`, runs `introspect::list_databases`, drops the slot.
- `list_schemas(server_id, database) -> Vec<String>` — cache-backed; on cache miss, runs a full introspection of the database and stores the payload.
- `list_relations(server_id, database, schema) -> Vec<RelationInfo>` — cache-backed; slices the cached payload for the requested schema.
- `list_functions(server_id, database, schema) -> Vec<FunctionInfo>` — cache-backed; slices the cached payload for the requested schema.
- `refresh_schema_cache(server_id, database) -> SchemaPayload` — explicit refresh; re-introspects and overwrites the cache row.

All five honour AGENTS.md principle 1 (every Postgres connection is the result of an explicit user action — here, the user expanded a node in the tree or clicked Refresh). Cache misses for `list_schemas`/`list_relations`/`list_functions` *do* acquire a slot (one query per first expand per DB), which is what makes the slot indicator visibly bump while introspection runs. A smoke test (`smoke_introspect.rs`) exercises the full cache-miss → cache-hit → refresh cycle against a real Postgres, gated on `QUILL_TEST_PG_URL`.

This task is **backend-only**. Per the agreed scope, M2.3 does not change `src/routes/+page.svelte` — the existing M1.6 connection-list UI keeps working unchanged. M2.4 will replace it with a tree that consumes the new commands. The typed bridge `src/lib/tauri.ts` is also untouched in M2.3; M2.4 owns adding the new methods.

## Current state

Every file below already exists and is reproduced in relevant excerpts. Read in full before writing anything.

### `src-tauri/src/commands/mod.rs`

Holds `CommandError`, `QueryResult`, `ColumnMeta`, `pg_row_to_json`, and seven commands (`list_connections`, `save_connection`, `delete_connection`, `connect_server`, `disconnect_server`, `run_query`, `get_slot_state`). The relevant excerpts:

```rust
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum CommandError {
    UnknownConnection(String),
    NotConnected(String),
    Slot(String),
    Pg(String),
    Store(String),
}

impl From<store::StoreError> for CommandError {
    fn from(e: store::StoreError) -> Self { Self::Store(e.to_string()) }
}

impl CommandError {
    fn unknown_connection(id: i64) -> Self { Self::UnknownConnection(format!("connection {id} not found")) }
    fn not_connected(id: i64) -> Self { Self::NotConnected(format!("not connected to server {id}")) }
}
```

`run_query`'s slot-acquisition pattern is the template the new commands follow:

```rust
let handle = registry.by_id.get(&server_id).ok_or_else(|| CommandError::not_connected(server_id))?;
let slot_manager = handle.slot_manager.clone();
drop(handle);    // never hold a DashMap shard lock across `.await`
let mut guard = slot_manager.acquire(&database).await.map_err(|e: SlotError| CommandError::Slot(e.to_string()))?;
let rows: Vec<PgRow> = sqlx::query(&sql).fetch_all(&mut *guard).await.map_err(|e| CommandError::Pg(e.to_string()))?;
// guard drops here — slot returns to idle
```

`State<'_, sqlx::SqlitePool>` and `State<'_, ServerRegistry>` are the two managed-state handles available; both are set up in `lib.rs::setup`.

### `src-tauri/src/introspect/mod.rs` (post-M2.2)

```rust
pub const PAYLOAD_VERSION: u32 = 1;

pub struct DatabaseInfo { pub name: String }

pub struct SchemaPayload { pub v: u32, pub schemas: Vec<SchemaInfo> }
pub struct SchemaInfo { pub name: String, pub relations: Vec<RelationInfo>, pub functions: Vec<FunctionInfo> }
pub struct RelationInfo { pub name: String, pub kind: RelationKind }
pub struct FunctionInfo { pub name: String, pub kind: FunctionKind }

pub enum RelationKind { Table, View, Matview, PartitionedTable }   // serde snake_case
pub enum FunctionKind { Function, Procedure, Aggregate, Window }    // serde snake_case

pub enum IntrospectError {
    Pg(sqlx::Error),
    UnknownRelKind(String),
    UnknownProKind(String),
}

pub async fn list_databases(conn: &mut PgConnection) -> Result<Vec<DatabaseInfo>, IntrospectError>;
pub async fn introspect_database(conn: &mut PgConnection) -> Result<SchemaPayload, IntrospectError>;
```

### `src-tauri/src/store/mod.rs` (post-M2.1)

```rust
pub struct SchemaCacheRow {
    pub server_id: i64,
    pub database: String,
    pub payload_json: String,
    pub fetched_at: String,
}

pub async fn get_schema_cache(pool: &SqlitePool, server_id: i64, database: &str) -> Result<Option<SchemaCacheRow>, StoreError>;
pub async fn set_schema_cache(pool: &SqlitePool, server_id: i64, database: &str, payload_json: &str) -> Result<SchemaCacheRow, StoreError>;
pub async fn delete_schema_cache(pool: &SqlitePool, server_id: i64, database: &str) -> Result<(), StoreError>;
pub async fn delete_schema_cache_for_server(pool: &SqlitePool, server_id: i64) -> Result<(), StoreError>;
```

### `src-tauri/src/lib.rs` (post-M2.2)

```rust
pub mod commands;
pub mod introspect;
pub mod pg;
pub mod registry;
pub mod slots;
pub mod store;

// ... setup + invoke_handler with the seven M1.5 commands ...
```

The `invoke_handler` list must be extended to include the five new commands (deliverable 3).

### `src-tauri/src/registry.rs`

```rust
pub struct ServerHandle { pub slot_manager: Arc<SlotManager<PgConnector>> }
pub struct ServerRegistry { pub by_id: DashMap<i64, ServerHandle> }
```

### `src-tauri/capabilities/default.json`

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Capability for the main window",
  "windows": ["main"],
  "permissions": ["core:default", "opener:default"]
}
```

`tauri:default` for crate commands is auto-resolved in the current setup — no change needed here in M2.3 (the M1.5 spec called for adding `"quill:default"` but the file was never updated and the app works regardless, suggesting Tauri 2 grants local commands by default in this configuration). Do **not** add anything to this file in M2.3 unless the new commands fail to invoke at runtime; if they do, add `"quill:default"` and document the symptom.

## Why `list_schemas`/`list_relations`/`list_functions` all read from the same cache

The MILESTONES seed context spells this out: one cache row per `(server, database)`, holding every schema, relation, and function for that DB. The three list commands all slice the same payload — they exist as separate commands only so the frontend can request the data it actually needs at each tree node without re-serializing the whole payload through IPC for an expand of a single schema.

Concretely:
- First expand of any node under DB `X` → cache miss on `(server_id, X)` → acquire slot → `introspect::introspect_database` → write `schema_cache` row → return the slice the caller asked for.
- Every subsequent `list_*` for `(server_id, X)` → cache hit → no slot acquisition, no Postgres I/O.
- `refresh_schema_cache(server_id, X)` → unconditionally re-introspect and overwrite.

A single internal helper `ensure_payload(server_id, database, &pool, &registry) -> SchemaPayload` is the right factoring; each list command calls it then projects.

## Deliverables

### 1. `src-tauri/src/commands/mod.rs` — add `Introspect` error variant

Add one variant to `CommandError`:

```rust
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum CommandError {
    UnknownConnection(String),
    NotConnected(String),
    Slot(String),
    Pg(String),
    Store(String),
    Introspect(String),    // <-- new
    UnknownDatabase(String), // <-- new — schema/relation/function asked for an uncached + unintrospectable DB
}
```

Extend the `Display` match arms accordingly:

```rust
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
```

Add a `From` impl for `IntrospectError`:

```rust
impl From<crate::introspect::IntrospectError> for CommandError {
    fn from(e: crate::introspect::IntrospectError) -> Self {
        Self::Introspect(e.to_string())
    }
}
```

Add a convenience constructor next to the existing ones:

```rust
impl CommandError {
    // ... existing fns ...
    fn unknown_database(server_id: i64, database: &str) -> Self {
        Self::UnknownDatabase(format!(
            "database '{database}' is not cached for server {server_id}; expand it in the tree or call refresh_schema_cache"
        ))
    }
}
```

Note: `UnknownDatabase` is for the narrow case where a stale frontend asks `list_schemas` for a DB it never expanded *and* the slot manager can't honour the implicit fetch (e.g. budget exhausted). For the normal flow, `ensure_payload` triggers introspection on cache miss and returns the payload; the variant exists so the frontend has a clean error to handle later.

### 2. `src-tauri/src/commands/mod.rs` — `ensure_payload` helper + five new commands

Append below the existing `get_slot_state` command, but above the `#[cfg(test)] mod tests`.

```rust
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
        let payload: SchemaPayload = serde_json::from_str(&row.payload_json).map_err(|e| {
            // Stale payload version or hand-edited cache — wipe and re-introspect
            // would be the friendly thing, but for v1 surface the error.
            CommandError::Introspect(format!("cached payload is unreadable: {e}"))
        })?;
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

    Ok(introspect::introspect_database(&mut *guard).await?)
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
    // Look up the saved server to know which DB to bind the slot to.  The
    // catalog query against pg_database returns the same rows regardless
    // of the connected DB, but we still have to *be* connected to some DB
    // to ask — `default_db` is the right choice because the user authorized
    // a connection to it when they saved the server.
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

    Ok(introspect::list_databases(&mut *guard).await?)
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
/// vec if the schema isn't present in the cached payload (caller must have
/// asked for a schema that doesn't exist on the server).
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
```

The `unknown_database` constructor is added in deliverable 1 for symmetry with M1.5's `unknown_connection` / `not_connected` constructors. M2.3 itself doesn't return that variant — every cache miss is recoverable via `ensure_payload`. M2.4 may surface it from the frontend's perspective if a stale UI requests a never-cached DB without first expanding it (the recovery path is just calling `list_schemas`, which will introspect implicitly). Leaving the variant in place is a one-line cost and saves an enum bump later.

### 3. `src-tauri/src/lib.rs` — register the five commands

Extend the `invoke_handler` macro arguments. Order doesn't matter to Tauri, but grouping the introspection commands together keeps the file readable:

```rust
.invoke_handler(tauri::generate_handler![
    commands::list_connections,
    commands::save_connection,
    commands::delete_connection,
    commands::connect_server,
    commands::disconnect_server,
    commands::run_query,
    commands::get_slot_state,
    commands::list_databases,
    commands::list_schemas,
    commands::list_relations,
    commands::list_functions,
    commands::refresh_schema_cache,
])
```

No other changes to `lib.rs`.

### 4. `src-tauri/tests/smoke_introspect.rs` — new file

```rust
//! Smoke test: full end-to-end flow through the introspection commands.
//!
//! Exercises the cache-miss → cache-hit → refresh cycle by calling the
//! same internal pieces the `#[tauri::command]` wrappers use, in the same
//! order, against a real Postgres + an in-memory SQLite store.
//!
//! Run with:
//!   QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh
//!
//! Without the env var, every test silently passes after a stderr note.

use secrecy::SecretString;

use quill_lib::introspect::{self, PAYLOAD_VERSION};
use quill_lib::pg::PgConnector;
use quill_lib::registry::{ServerHandle, ServerRegistry};
use quill_lib::store;

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
    eprintln!("QUILL_TEST_PG_URL not set; skipping smoke_introspect test");
}

async fn fresh_pool_and_server(dsn: &TestDsn) -> (sqlx::SqlitePool, ServerRegistry, i64) {
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
        .expect("in-memory sqlite");
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrations");

    let conn = store::insert(
        &pool,
        store::NewConnection {
            name: "smoke-introspect".into(),
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

    let registry = ServerRegistry::default();
    let connector = PgConnector {
        host: dsn.host.clone(),
        port: dsn.port,
        username: dsn.username.clone(),
        password: SecretString::from(dsn.password.clone()),
        ssl_mode: sqlx::postgres::PgSslMode::Disable,
    };
    let handle = ServerHandle::new(connector, conn.slot_budget.max(1) as usize);
    registry.by_id.insert(conn.id, handle);

    (pool, registry, conn.id)
}

#[tokio::test]
async fn list_databases_returns_postgres() {
    let Some(dsn) = dsn() else { skip_note(); return };
    let (_pool, registry, server_id) = fresh_pool_and_server(&dsn).await;

    let handle = registry.by_id.get(&server_id).expect("handle");
    let mgr = handle.slot_manager.clone();
    drop(handle);

    let mut guard = mgr.acquire(&dsn.database).await.expect("acquire");
    let dbs = introspect::list_databases(&mut *guard).await.expect("list_databases");
    drop(guard);

    let names: Vec<&str> = dbs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"postgres"), "got {names:?}");

    mgr.disconnect_all().await;
}

#[tokio::test]
async fn ensure_payload_misses_then_hits() {
    let Some(dsn) = dsn() else { skip_note(); return };
    let (pool, registry, server_id) = fresh_pool_and_server(&dsn).await;

    // Cache is empty.
    assert!(store::get_schema_cache(&pool, server_id, &dsn.database)
        .await
        .unwrap()
        .is_none());

    // ── Cache-miss path: replicate `ensure_payload` inline ────────────
    let handle = registry.by_id.get(&server_id).expect("handle");
    let mgr = handle.slot_manager.clone();
    drop(handle);

    let mut guard = mgr.acquire(&dsn.database).await.expect("acquire");
    let payload1 = introspect::introspect_database(&mut *guard)
        .await
        .expect("introspect");
    drop(guard);

    let json = serde_json::to_string(&payload1).expect("serialize");
    store::set_schema_cache(&pool, server_id, &dsn.database, &json)
        .await
        .expect("set_schema_cache");

    // Cache now populated.
    let row = store::get_schema_cache(&pool, server_id, &dsn.database)
        .await
        .unwrap()
        .expect("row exists");
    assert!(!row.fetched_at.is_empty());

    // ── Cache-hit path: deserialize the row, should match ─────────────
    let payload2: introspect::SchemaPayload =
        serde_json::from_str(&row.payload_json).expect("deserialize");
    assert_eq!(payload1, payload2);
    assert_eq!(payload2.v, PAYLOAD_VERSION);

    mgr.disconnect_all().await;
}

#[tokio::test]
async fn refresh_overwrites_existing_cache() {
    let Some(dsn) = dsn() else { skip_note(); return };
    let (pool, registry, server_id) = fresh_pool_and_server(&dsn).await;

    // Seed a deliberately-bogus payload so we can verify it gets replaced.
    store::set_schema_cache(
        &pool,
        server_id,
        &dsn.database,
        r#"{"v":1,"schemas":[{"name":"stale","relations":[],"functions":[]}]}"#,
    )
    .await
    .expect("seed");

    let before = store::get_schema_cache(&pool, server_id, &dsn.database)
        .await
        .unwrap()
        .unwrap();
    assert!(before.payload_json.contains("stale"));

    // Refresh.
    let handle = registry.by_id.get(&server_id).expect("handle");
    let mgr = handle.slot_manager.clone();
    drop(handle);

    let mut guard = mgr.acquire(&dsn.database).await.expect("acquire");
    let fresh = introspect::introspect_database(&mut *guard)
        .await
        .expect("introspect");
    drop(guard);

    let json = serde_json::to_string(&fresh).expect("serialize");
    store::set_schema_cache(&pool, server_id, &dsn.database, &json)
        .await
        .expect("overwrite");

    let after = store::get_schema_cache(&pool, server_id, &dsn.database)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !after.payload_json.contains(r#""stale""#),
        "stale schema should have been overwritten; got: {}",
        after.payload_json
    );
    // `public` is on every stock cluster — useful sentinel for "real".
    assert!(after.payload_json.contains("public"));

    mgr.disconnect_all().await;
}

#[tokio::test]
async fn command_error_introspect_serde_shape() {
    // Pure unit test — runs without QUILL_TEST_PG_URL.
    let err = quill_lib::commands::CommandError::Introspect("boom".into());
    let json = serde_json::to_value(err).expect("serialize");
    assert_eq!(json["kind"], "Introspect");
    assert_eq!(json["message"], "boom");

    let err = quill_lib::commands::CommandError::UnknownDatabase(
        "database 'nope' is not cached for server 1".into(),
    );
    let json = serde_json::to_value(err).expect("serialize");
    assert_eq!(json["kind"], "UnknownDatabase");
}
```

## Implementation order

There are no intermediate compile errors if you follow this sequence.

1. **`src-tauri/src/commands/mod.rs`** — apply deliverable 1 (error variants + From impl) and deliverable 2 (helpers + five commands). Verify `( cd src-tauri && cargo build )` succeeds.
2. **`src-tauri/src/lib.rs`** — extend the `invoke_handler` per deliverable 3. Verify `( cd src-tauri && cargo build )` still succeeds (this is the first point Tauri checks every command is registered).
3. **`src-tauri/tests/smoke_introspect.rs`** — write the smoke test. Run `./test.sh` to confirm everything still compiles, unit tests pass, and the new integration tests cleanly skip without `QUILL_TEST_PG_URL`.
4. Optionally: start a Docker Postgres and re-run with `QUILL_TEST_PG_URL` set to verify the real cache-miss → cache-hit → refresh cycle.

## Known gotchas

- **Holding `DashMap::Ref` across `.await` deadlocks.** Same rule as M1.5: clone the `Arc<SlotManager>` out of the map, drop the `Ref`, then await. `ensure_payload`, `run_introspection`, and `list_databases` all follow this pattern. The bug surfaces as a hang, not an error — easy to miss in a one-shot test.
- **`list_databases` needs to know which DB to bind the slot to.** `pg_database` is server-wide, but you still have to be connected to *some* DB to query it. The command reads `connections.default_db` from the local store. This is the only place that touches the SQLite store *and* needs the registry — keep the order: load the row first, then check `not_connected`, so a typo'd server id surfaces as `UnknownConnection` not `NotConnected`.
- **Cache hit does not acquire a slot.** This is the principle 1 win: `list_schemas("postgres")` on a warm cache is pure SQLite — no Postgres I/O, no slot bump in the UI. Verify by reading the code: `ensure_payload` returns early on cache hit, never touching `registry`.
- **Cache miss acquires *one* slot for the *whole* DB.** All schemas, relations, functions in one round-trip via `introspect::introspect_database`. The slot indicator visibly bumps `[1/2]` briefly during this call. Don't split into three commands that each acquire a slot — that's three round-trips and three slot-indicator flickers for what the user perceives as one "expand DB" action.
- **`refresh_schema_cache` *always* acquires a slot**, even if the cache row is fresh. That's the contract: the user clicked Refresh, so we re-fetch.
- **Cache reads of malformed JSON return `CommandError::Introspect`.** This can happen if a developer hand-edited the SQLite file, or if a future PAYLOAD_VERSION bump lands without a migration. In v1 we surface the error rather than silently re-introspecting; the user-facing fix is "click Refresh." Document this in the variant message.
- **`store::get_schema_cache` returns `Option<SchemaCacheRow>`, not `Result<Option<...>, ...>`** — wait, it does return `Result<Option<_>>`. Use `?` to bubble the StoreError and `if let Some(row)` to branch on the option.
- **`tauri::State<'_, T>` lifetime constraints.** `ensure_payload` and `run_introspection` take `&sqlx::SqlitePool` and `&ServerRegistry` directly (not `State`); the command functions extract `&*pool` and `&*registry` from the `State` wrappers. This keeps the helpers reusable from non-Tauri contexts (the smoke test).
- **`State<'_, sqlx::SqlitePool>::deref()` returns `&sqlx::SqlitePool`.** Pass `&pool` to helpers — `&*pool` is also valid but more characters; either works. Same for `&registry`.
- **`introspect::IntrospectError: From` does not auto-convert into `sqlx::Error`.** The `From` impl in deliverable 1 maps it to `CommandError::Introspect`. Inside `ensure_payload`, the `?` after `introspect::introspect_database(&mut *guard).await` uses that From impl.
- **The `introspect::Database` (oops, `DatabaseInfo`) struct is `Serialize`, so returning `Vec<DatabaseInfo>` from a Tauri command works out of the box.** No wrapper type needed. Same for `RelationInfo`, `FunctionInfo`, and `SchemaPayload`.
- **Tauri 2 command argument casing.** `server_id` becomes `serverId` in the JS invoke key; struct fields inside `RelationInfo`/`FunctionInfo` etc. stay snake_case. The M2.4 TypeScript bridge will need this exact split.
- **The smoke test's `fresh_pool_and_server` enables `PRAGMA foreign_keys = ON`.** This mirrors what M2.1's production `open` does. Without it, the cache test wouldn't exercise the cascade; with it, even the cache tests are honest about FK behaviour.
- **`Vec<RelationInfo>` ordering** — comes from the SQL `ORDER BY` in M2.2. Tests should not depend on insertion order from the catalog's natural sort if it differs from the SQL sort, but both `pg_class` and `pg_proc` are queried with explicit `ORDER BY nspname, relname`/`proname`, so the order is deterministic per schema after the `BTreeMap` groupby.
- **Empty schemas are returned.** A schema with no relations and no functions still shows up in `list_schemas` (M2.2 includes it in `BTreeMap` even with empty `relations`/`functions` vecs). `list_relations` and `list_functions` on such a schema return empty vecs — not an error.
- **No tests for the `unknown_database` variant.** It's not reachable from the v1 command surface — `ensure_payload` always populates on miss. The variant exists for M2.4's frontend to handle in the rare case where the user navigates faster than the cache writes; covered in M2.4's spec.
- **Do not modify `src/lib/tauri.ts` or `src/routes/+page.svelte` in this task.** M2.4 owns the frontend additions. The existing M1.6 UI doesn't call the new commands — they exist on the backend, unreachable from the UI, which is intentional.

## Tests

Run via `./test.sh` (and with `QUILL_TEST_PG_URL` set against a real Postgres for the smoke tests). Coverage:

**Pure unit test (runs always):**
- `command_error_introspect_serde_shape` — `CommandError::Introspect` and `CommandError::UnknownDatabase` serialize to the expected `{kind, message}` shape.

**Integration tests (skipped without `QUILL_TEST_PG_URL`):**
- `list_databases_returns_postgres` — proves the slot-bound `list_databases` call returns the canonical `postgres` DB.
- `ensure_payload_misses_then_hits` — cache empty → introspect + write → cache populated → second read returns equal payload.
- `refresh_overwrites_existing_cache` — seeds a fake "stale" payload, calls the refresh path, asserts the stale schema is gone and `public` is back.

The smoke tests intentionally don't call `#[tauri::command]` functions directly (`State<'_, _>` is only constructable inside a Tauri runtime) — they replicate the helper code path inline, which is what makes them a *smoke* test rather than a unit test.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds without `QUILL_TEST_PG_URL` — the four smoke-test cases either pass (the pure unit one) or skip cleanly.
- [ ] `QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh` succeeds against a local `postgres:17` Docker container, with every smoke test reporting `ok` plus the existing `pg_integration`, `smoke_select_1`, and `introspect_integration` suites still passing.
- [ ] `grep -c "#\[tauri::command\]" src-tauri/src/commands/mod.rs` returns `12` (7 from M1.5 + 5 new).
- [ ] `grep -n "invoke_handler" src-tauri/src/lib.rs` shows all 12 commands registered.
- [ ] `grep -c "ensure_payload\|run_introspection" src-tauri/src/commands/mod.rs` is at least `4` — both helpers defined and both used (`ensure_payload` from each of the three slicing commands; `run_introspection` from `ensure_payload` and `refresh_schema_cache`).
- [ ] No changes to `src/`, no changes to `src-tauri/migrations/`, no changes to `src-tauri/capabilities/`.
- [ ] Exactly one new file under `src-tauri/tests/`: `smoke_introspect.rs`.
- [ ] `git diff src-tauri/src/commands/mod.rs` shows the two new error variants, the new `From<IntrospectError>` impl, the new constructor, the two helpers, and the five new `#[tauri::command]` functions — nothing else (no churn to existing commands).
- [ ] On a warm cache (second call), `list_schemas` does **not** acquire a Postgres slot. Verify by reading `ensure_payload`: the `if let Some(row) = ...` branch returns before reaching `run_introspection`.

## Out of scope

- Frontend code — **M2.4**.
- The typed bridge `src/lib/tauri.ts` gaining methods for the five new commands — **M2.4**.
- Column metadata in the payload — **M4** (`PAYLOAD_VERSION` bump).
- A migration to re-introspect existing v1 payloads on first read post-bump — **M4**.
- `search_path` capture and storage — **M4**.
- Per-schema partial refresh — `refresh_schema_cache` is the only refresh path; it operates per-DB. `MILESTONES.md` accepts the "Refresh on any node under a DB re-introspects that DB" behaviour explicitly.
- TTL-based or background cache invalidation — explicit non-goal per AGENTS.md principle 1.
- Cache invalidation on `disconnect_server` — M2.3 leaves cache rows in place; the user can connect again later and pick up where they left off. `delete_schema_cache_for_server` is exposed in the store but isn't called by any command in M2.
- A `get_schema_cache_freshness(server_id, db)` command — the `fetched_at` timestamp is readable directly by M2.4 if it wants to surface "cached 2 hours ago" hints (not required by `PRD.md`).
- Cancellation of an in-flight introspection — **M3** (cancellation is its whole milestone).
