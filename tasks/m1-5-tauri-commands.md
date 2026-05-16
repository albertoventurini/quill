# M1.5 — Tauri command surface + hardcoded `SELECT 1` smoke test

## Goal
Expose a typed Tauri command surface to the frontend, and prove the whole stack works by running `SELECT 1` end-to-end against a seeded connection — **before any UI exists**.

## Context to read first
- `PRD.md` §5 (architecture / backend modules), §6 (slot model).
- `AGENTS.md` — style and design principles.
- `tasks/m1-2-store.md`, `tasks/m1-3-slot-manager.md`, `tasks/m1-4-postgres-binding.md` — this task composes their outputs.

## Deliverables

### 1. Commands
`src-tauri/src/commands/mod.rs`:

```rust
#[tauri::command]
pub async fn list_connections(
    pool: State<'_, SqlitePool>,
) -> Result<Vec<Connection>, CommandError>;

#[tauri::command]
pub async fn save_connection(
    new: NewConnection,
    pool: State<'_, SqlitePool>,
) -> Result<Connection, CommandError>;

#[tauri::command]
pub async fn delete_connection(
    id: i64,
    pool: State<'_, SqlitePool>,
) -> Result<(), CommandError>;

#[tauri::command]
pub async fn connect_server(
    id: i64,
    password: String,
    pool: State<'_, SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<SlotState, CommandError>;

#[tauri::command]
pub async fn disconnect_server(
    id: i64,
    registry: State<'_, ServerRegistry>,
) -> Result<(), CommandError>;

#[tauri::command]
pub async fn run_query(
    server_id: i64,
    database: String,
    sql: String,
    registry: State<'_, ServerRegistry>,
) -> Result<QueryResult, CommandError>;

#[tauri::command]
pub fn get_slot_state(
    server_id: i64,
    registry: State<'_, ServerRegistry>,
) -> Result<Option<SlotState>, CommandError>;
```

### 2. Types
```rust
#[derive(Serialize)]
pub struct QueryResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub duration_ms: u64,
}

#[derive(Serialize)]
pub struct ColumnMeta { pub name: String, pub type_name: String }

#[derive(thiserror::Error, Debug, Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum CommandError {
    #[error("connection {0} not found")]    UnknownConnection(i64),
    #[error("not connected to server {0}")] NotConnected(i64),
    #[error("slot: {0}")]                   Slot(String),
    #[error("postgres: {0}")]               Pg(String),
    #[error("store: {0}")]                  Store(String),
}
```
(Map `SlotError`, `sqlx::Error`, `StoreError` into the string-payload variants — keeps the surface stable and serde-friendly.)

### 3. Row → JSON conversion
Helper `pg_row_to_json(&PgRow) -> Vec<serde_json::Value>` that switches on the column's type OID/name. For M1, support: `bool`, `int2/4/8`, `float4/8`, `text/varchar/name`, `date`, `time`, `timestamp`, `timestamptz`, `uuid`, `json`, `jsonb`, `bytea` (encode as base64 string). For anything else, fall back to a best-effort `String` representation — **never error**; the user wants to see something.

### 4. `connect_server` semantics
1. Load the `Connection` from the store; return `UnknownConnection` if missing.
2. Build a `PgConnector` from the row + the supplied `password`.
3. If `registry.by_id` already has an entry for `id`, reuse it (the password is ignored — already running). Otherwise build a new `SlotManager` with `slot_budget`.
4. Return the current `SlotState`. **Do not eagerly open any connection** — connections happen on `run_query` (principle 1).

### 5. `run_query` semantics
1. Look up `ServerHandle` in the registry; return `NotConnected` if absent.
2. `slot_manager.acquire(&database).await?` — this is the only place that may open a Postgres connection.
3. Run the SQL via `sqlx::query(&sql).fetch_all(&mut *conn)`.
4. Convert rows; return `QueryResult` with timing.
5. Surface any error as `CommandError::Pg(...)`.

### 6. Register
Edit `src-tauri/src/lib.rs`:
```rust
.invoke_handler(tauri::generate_handler![
    commands::list_connections,
    commands::save_connection,
    commands::delete_connection,
    commands::connect_server,
    commands::disconnect_server,
    commands::run_query,
    commands::get_slot_state,
])
```
Ensure `src-tauri/capabilities/default.json` permits these (Tauri 2 needs explicit permissions).

## Smoke test (no UI)
`src-tauri/tests/smoke_select_1.rs`, gated on `QUILL_TEST_PG_URL`:
1. `SqlitePool::connect("sqlite::memory:")`, run migrations.
2. Insert a `connections` row pointing at the test DSN.
3. Build a `ServerRegistry`.
4. Call the command functions **directly** (not through Tauri IPC).
5. `connect_server(id, password)` → assert `SlotState.slots.is_empty()` (no eager open).
6. `run_query(id, "postgres", "SELECT 1 AS one")` → `rows == [[Value::Number(1)]]`, `columns[0].name == "one"`.
7. `get_slot_state(id)` → one slot, idle, bound to `"postgres"`.
8. `disconnect_server(id)` → registry entry removed; subsequent `run_query` → `NotConnected`.

## Acceptance criteria
- [ ] All commands compile, are registered, and have capability entries.
- [ ] `cargo test smoke_select_1` with `QUILL_TEST_PG_URL` set passes against local Docker Postgres.
- [ ] `CommandError` serializes to a discriminated union (`{ "kind": "Pg", "message": "..." }`) that the frontend can pattern-match on.
- [ ] No connection is opened by `connect_server` itself (lazy open per principle 1).
- [ ] `./test.sh` passes (unit tests still green; smoke test gated on env var).

## Out of scope
- UI consuming these commands — M1.6.
- Query cancellation — M3.
- Streamed/paginated results — M3.
- Schema introspection commands — M2.
