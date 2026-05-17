# M1.4 — Postgres binding for slot manager

## Goal

**Before:** The slot manager (`src-tauri/src/slots/mod.rs`) is fully working but tested only against an in-memory `FakeConnector`. No code in the crate can open a real Postgres connection. There is no per-server registry — the app has no place to hold live `SlotManager`s keyed by saved-connection id.

**After:** A `PgConnector` (in `src-tauri/src/pg/mod.rs`) implements the `slots::Connector` trait using `sqlx::PgConnection`. A `ServerRegistry` (in `src-tauri/src/registry.rs`) holds one `SlotManager<PgConnector>` per connected server, wired into Tauri as managed state during `setup`. `SlotGuard` gains a `DerefMut` impl (a five-line patch to `slots/mod.rs`) so callers can actually run sqlx queries through it. Integration tests (gated on `QUILL_TEST_PG_URL`) prove the connector opens a real Postgres connection, runs `SELECT 1`, and respects the slot budget with real sockets — including LRU eviction. No Tauri commands are exposed yet; that is M1.5.

## Current state

The files below already exist and will be modified. Read them in full before touching anything else — the trait you must implement, the dependency layout, and the wiring point in `lib.rs` are all here verbatim.

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
sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio", "macros", "migrate"] }
async-trait = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }
thiserror = "2"
```

### `src-tauri/src/lib.rs`

```rust
mod slots;
mod store;

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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

### `src-tauri/src/slots/mod.rs` (relevant excerpt — the trait you must implement)

```rust
/// Errors from the connector when establishing a connection.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ConnectorError(pub String);

/// A trait for creating and destroying database connections.
///
/// `close` is best-effort and may swallow errors.
#[async_trait]
pub trait Connector: Send + Sync + 'static {
    type Conn: Send + 'static;

    /// Open a new connection to `database`.
    async fn connect(&self, database: &str) -> Result<Self::Conn, ConnectorError>;

    /// Close a previously-opened connection (best-effort).
    async fn close(conn: Self::Conn);
}
```

The full slot-manager module is at `src-tauri/src/slots/mod.rs` and is ~830 lines including its tests. The trait above is what you must implement. `Slot`, `SlotManager`, the `Recovery` drop-guard for cancellation safety, and the four-rule acquisition logic are complete — do not modify them. The **only** patch to this file is the addition of a `DerefMut` impl for `SlotGuard` (see deliverable 1b below); without it, callers cannot pass `&mut *guard` to sqlx executors.

The current `SlotGuard` impl block for reference:

```rust
impl<C: Connector> Deref for SlotGuard<'_, C> {
    type Target = C::Conn;

    fn deref(&self) -> &C::Conn {
        self.conn
            .as_ref()
            .expect("SlotGuard always holds a connection")
    }
}

impl<C: Connector> AsRef<C::Conn> for SlotGuard<'_, C> {
    fn as_ref(&self) -> &C::Conn {
        self.conn
            .as_ref()
            .expect("SlotGuard always holds a connection")
    }
}
```

### `src-tauri/src/store/mod.rs` (relevant excerpt — style precedent, do not modify)

```rust
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("Migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub async fn open(app: &tauri::AppHandle) -> Result<SqlitePool, StoreError> { /* … */ }
```

Use this as the model for `pg`'s error/module shape (`thiserror`, top-level free functions, no struct-method noise where a free function is clearer).

### `src-tauri/src/main.rs`

```rust
// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    quill_lib::run()
}
```

Not modified by this task — listed only so you know the binary entry point delegates to `lib.rs::run`.

### `src-tauri/migrations/0001_initial.sql`

Already in place from M1.2; do not touch.

## Deliverables

### 1. `src-tauri/Cargo.toml` — extend dependencies

Modify three lines and add a `[dev-dependencies]` section.

- Change the `sqlx` line — add `"postgres"` as the **first** feature (so the `postgres` feature appears alongside `sqlite`):

  ```toml
  sqlx = { version = "0.8", features = ["postgres", "sqlite", "runtime-tokio", "macros", "migrate"] }
  ```

- Add two new dependency lines below `thiserror = "2"`:

  ```toml
  secrecy = "0.10"
  dashmap = "6"
  ```

- Add a new section at the bottom of the file:

  ```toml
  [dev-dependencies]
  url = "2"
  ```

  `url` is needed only by the integration tests in step 5 to parse `QUILL_TEST_PG_URL`.

Do **not** add `tracing`, `tracing-subscriber`, or any logging crate — the project has not adopted one yet.

### 1b. `src-tauri/src/slots/mod.rs` — add `DerefMut` for `SlotGuard`

Just below the existing `impl<C: Connector> Deref for SlotGuard<'_, C>` block (around line 156), add:

```rust
impl<C: Connector> std::ops::DerefMut for SlotGuard<'_, C> {
    fn deref_mut(&mut self) -> &mut C::Conn {
        self.conn
            .as_mut()
            .expect("SlotGuard always holds a connection")
    }
}
```

Rationale: `sqlx::Executor` is implemented for `&mut PgConnection`. Without `DerefMut`, `&mut *guard` fails to compile, and the slot guard is useless for any real query. M1.5's `run_query` will need this; the integration tests in step 5 need it too. Add no other helper methods; the existing `Deref` + `AsRef` + new `DerefMut` is the full surface.

Do not add a test for this in `slots/mod.rs` — it is a one-line compiler-checked impl, and the integration tests in step 5 exercise it end-to-end.

### 2. `src-tauri/src/pg/mod.rs` — new file

Create the file. Implement `PgConnector` and its `Connector` impl. The public shape is:

```rust
//! Real Postgres `Connector` implementation for the slot manager.
//!
//! See AGENTS.md principle 2: the slot manager *is* the connection pool.
//! This module deliberately uses a raw `PgConnection`, never `PgPool` —
//! a pool would defeat the budget by opening connections behind our back.

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use sqlx::Connection as _;                       // brings `.close()` into scope on PgConnection
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgConnection;

use crate::slots::{Connector, ConnectorError};

/// Connection parameters for a single saved server.
///
/// The password is held as a `SecretString` so it never appears in `Debug`
/// output or in panic messages.  M6 will populate this from the OS keychain;
/// in M1 it comes from the user's in-process input via the `connect_server`
/// command (which lands in M1.5).
pub struct PgConnector {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: SecretString,
    pub ssl_mode: PgSslMode,
}

impl PgConnector {
    /// Map the textual `ssl_mode` stored in the SQLite `connections` table
    /// to the typed `PgSslMode`.  Accepts the same spellings as libpq.
    pub fn parse_ssl_mode(s: &str) -> Result<PgSslMode, ConnectorError> {
        match s {
            "disable"     => Ok(PgSslMode::Disable),
            "allow"       => Ok(PgSslMode::Allow),
            "prefer"      => Ok(PgSslMode::Prefer),
            "require"     => Ok(PgSslMode::Require),
            "verify-ca"   => Ok(PgSslMode::VerifyCa),
            "verify-full" => Ok(PgSslMode::VerifyFull),
            other => Err(ConnectorError(format!("unknown ssl_mode: {other}"))),
        }
    }
}

#[async_trait]
impl Connector for PgConnector {
    type Conn = PgConnection;

    async fn connect(&self, database: &str) -> Result<Self::Conn, ConnectorError> {
        let opts = PgConnectOptions::new()
            .host(&self.host)
            .port(self.port)
            .database(database)
            .username(&self.username)
            .password(self.password.expose_secret())
            .ssl_mode(self.ssl_mode)
            .application_name("quill");

        PgConnection::connect_with(&opts)
            .await
            .map_err(|e| ConnectorError(e.to_string()))
    }

    async fn close(conn: Self::Conn) {
        // Best-effort: ignore errors per the trait contract.
        let _ = conn.close().await;
    }
}

// TODO(M3): Cancellation plumbing.  Postgres exposes an out-of-band
// `CancelRequest` mechanism that uses the backend PID + secret returned
// during connection startup.  sqlx 0.8 does not expose those fields on
// `PgConnection`, so capturing them requires either a wrapper around
// `PgConnection` or a parallel low-level connect path.  M3 owns this.
// Do *not* invent a half-finished cancel API in M1.
```

Notes on what this file does **not** do:
- No `cancel_token()` method — that is M3's surface; defining a stub now invites a wrong shape.
- No conversion *from* a `store::Connection` row — that wiring is M1.5's job. `PgConnector` is built directly from the password the user just typed plus the row's other fields.
- No retry, no `application_name` other than `"quill"`, no statement-logging configuration — keep the connect path minimal.

### 3. `src-tauri/src/registry.rs` — new file

Create the file with this content:

```rust
//! Per-process registry of live server connections.
//!
//! One `ServerHandle` per *connected* saved server (keyed by the SQLite
//! `connections.id`).  The registry stays empty until M1.5's
//! `connect_server` command inserts an entry.

use std::sync::Arc;

use dashmap::DashMap;

use crate::pg::PgConnector;
use crate::slots::SlotManager;

/// Live handle for one connected server.
///
/// The `SlotManager` is wrapped in `Arc` so command handlers can clone it
/// out of the map and use it without holding a `DashMap` shard lock across
/// an `.await`.
#[derive(Clone)]
pub struct ServerHandle {
    pub slot_manager: Arc<SlotManager<PgConnector>>,
}

impl ServerHandle {
    pub fn new(connector: PgConnector, budget: usize) -> Self {
        Self {
            slot_manager: Arc::new(SlotManager::new(connector, budget)),
        }
    }
}

/// Registered as Tauri managed state.  Empty at startup.
#[derive(Default)]
pub struct ServerRegistry {
    pub by_id: DashMap<i64, ServerHandle>,
}
```

### 4. `src-tauri/src/lib.rs` — wire in the new modules

Replace the file with the version below. Three changes from the current file:

- Add `pub mod pg;` and `pub mod registry;` (the integration tests in step 5 need these to be `pub`; promote `slots` and `store` to `pub` too while you are here, for consistency and so future tests can reach them).
- In `setup`, after `app.manage(pool);`, add `app.manage(registry::ServerRegistry::default());`.
- Leave the `greet` command and `invoke_handler` exactly as-is — M1.5 will replace them.

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

### 5. `src-tauri/tests/pg_integration.rs` — new file

Create the file. These tests are gated on `QUILL_TEST_PG_URL`; when the env var is absent they early-return so a plain `./test.sh` on a developer machine without Postgres still passes.

Use exactly this skeleton:

```rust
//! Integration tests for `PgConnector` against a real Postgres.
//!
//! Run with:
//!   QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh
//!
//! Without the env var, every test in this file silently passes after a
//! one-line note on stderr.  This keeps the suite green on machines that
//! do not have Postgres handy.
//!
//! The tests use only `postgres` and `template1`, both of which exist on a
//! stock cluster and accept connections by default.  No `CREATE DATABASE`
//! is required; the test user only needs CONNECT on those two DBs.

use secrecy::SecretString;

use quill_lib::pg::PgConnector;
use quill_lib::slots::{Connector, SlotManager};

/// Parsed test-DSN pieces.  Built from `QUILL_TEST_PG_URL`.
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
    eprintln!("QUILL_TEST_PG_URL not set; skipping pg_integration test");
}

fn connector_from(dsn: &TestDsn) -> PgConnector {
    PgConnector {
        host: dsn.host.clone(),
        port: dsn.port,
        username: dsn.username.clone(),
        password: SecretString::from(dsn.password.clone()),
        ssl_mode: sqlx::postgres::PgSslMode::Disable,
    }
}

#[tokio::test]
async fn connector_runs_select_one() {
    let Some(dsn) = dsn() else { skip_note(); return };

    let connector = connector_from(&dsn);
    let mut conn = connector.connect(&dsn.database).await.expect("connect");

    let (n,): (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&mut conn)
        .await
        .expect("SELECT 1");
    assert_eq!(n, 1);

    PgConnector::close(conn).await;
}

#[tokio::test]
async fn slot_manager_opens_two_distinct_databases() {
    let Some(dsn) = dsn() else { skip_note(); return };

    let mgr = SlotManager::new(connector_from(&dsn), 2);

    let mut g1 = mgr.acquire("postgres").await.expect("acquire postgres");
    let mut g2 = mgr.acquire("template1").await.expect("acquire template1");

    // Verify each guard's connection actually talks to the requested DB.
    // A bug where the database string was dropped or swapped during slot
    // binding would surface here.  `&mut *g` relies on the new `DerefMut`.
    let (db1,): (String,) = sqlx::query_as("SELECT current_database()")
        .fetch_one(&mut *g1)
        .await
        .expect("current_database on g1");
    assert_eq!(db1, "postgres");

    let (db2,): (String,) = sqlx::query_as("SELECT current_database()")
        .fetch_one(&mut *g2)
        .await
        .expect("current_database on g2");
    assert_eq!(db2, "template1");

    drop(g1);
    drop(g2);
    mgr.disconnect_all().await;
}

#[tokio::test]
async fn slot_manager_lru_evicts_with_budget_one() {
    let Some(dsn) = dsn() else { skip_note(); return };

    // budget=1 forces eviction on every cross-database acquire — no need
    // for a third real database (which a stock cluster doesn't have).
    let mgr = SlotManager::new(connector_from(&dsn), 1);

    let g = mgr.acquire("postgres").await.expect("acquire postgres");
    drop(g); // idle, bound to `postgres`

    // Rule 3: the only idle slot is bound to a different DB, so it must be
    // evicted (closed) and rebound to `template1`.  A regression where the
    // slot was reused without re-binding would surface as the next query
    // returning `postgres` instead of `template1`.
    let mut g = mgr.acquire("template1").await.expect("acquire template1");
    let (db,): (String,) = sqlx::query_as("SELECT current_database()")
        .fetch_one(&mut *g)
        .await
        .expect("current_database after eviction");
    assert_eq!(db, "template1");
    drop(g);

    // Evict again, back to `postgres`.
    let mut g = mgr.acquire("postgres").await.expect("acquire postgres again");
    let (db,): (String,) = sqlx::query_as("SELECT current_database()")
        .fetch_one(&mut *g)
        .await
        .expect("current_database after second eviction");
    assert_eq!(db, "postgres");
    drop(g);

    mgr.disconnect_all().await;
}

#[tokio::test]
async fn bad_password_returns_connect_error() {
    let Some(mut dsn) = dsn() else { skip_note(); return };
    dsn.password.push_str("-wrong");

    let connector = connector_from(&dsn);
    let result = connector.connect(&dsn.database).await;
    assert!(result.is_err(), "expected auth failure, got Ok");
}
```

Notes:
- Both `&mut *g1` patterns rely on the `DerefMut` impl added in deliverable 1b. Without it the file will not compile.
- All four tests use only `postgres` and `template1`. Both ship on every stock Postgres install and accept connections out of the box; no privileged setup is needed beyond the Docker container in the run command above.
- No `sqlx::Executor` import is needed — `sqlx::query_as` takes the executor by value, so the type-class lookup runs through `&mut PgConnection` automatically once `DerefMut` is in place.

## Implementation order

Touch files in this order. Until step 5, the new modules exist on disk but are not in the crate's module tree, so `cargo` will not compile them. There are **no intermediate compile errors** if you follow this sequence.

1. **`src-tauri/Cargo.toml`** — add `postgres` feature, add `secrecy`, `dashmap`, dev-dep `url`. Verify with `( cd src-tauri && cargo build )` — should still build the existing code, just with extra deps downloaded.
2. **`src-tauri/src/slots/mod.rs`** — add the `DerefMut` impl from deliverable 1b. Run `( cd src-tauri && cargo test slots )` to make sure the slot manager's own tests still pass.
3. **`src-tauri/src/pg/mod.rs`** — write the new file. Not yet compiled (no `mod pg;` in `lib.rs`).
4. **`src-tauri/src/registry.rs`** — write the new file. Not yet compiled.
5. **`src-tauri/src/lib.rs`** — promote modules to `pub mod`, add `pub mod pg;` and `pub mod registry;`, and add `app.manage(registry::ServerRegistry::default());` to `setup`. Run `( cd src-tauri && cargo build )` — must succeed; this is the first point everything is type-checked together.
6. **`src-tauri/tests/pg_integration.rs`** — write the integration tests. Run `./test.sh` to confirm the unit tests still pass and the integration tests cleanly skip without `QUILL_TEST_PG_URL`. Then start a Docker Postgres and re-run with the env var set.

## Known gotchas

- **`secrecy` 0.10 constructor changed.** Older docs show `SecretString::new(String::from("x"))`; in 0.10 the constructor takes `Box<str>`. Use `SecretString::from("x")` (via the `From<&str>` impl) or `SecretString::from(String::from("x"))` (via the `From<String>` impl). The test code above uses `SecretString::from(string)` — that is correct on 0.10.
- **`ExposeSecret` is a trait import, not a method.** You **must** `use secrecy::ExposeSecret;` for `self.password.expose_secret()` to resolve. Without it, the error reads "no method named `expose_secret`" with no hint that a trait import is missing.
- **`sqlx::Connection` is a trait whose name collides with `store::Connection` (the struct).** Inside `pg/mod.rs` there is no collision because the struct isn't imported, but always use `use sqlx::Connection as _;` (anonymous trait import) to bring the `.close()` method into scope without shadowing anything. The trait must be in scope or `conn.close()` won't compile.
- **Do not use `PgPool`.** The whole point of the slot manager is to *be* the pool. Using `sqlx::PgPool` here — even "just for the test" — would open connections behind the slot manager's back and violate AGENTS.md principle 2. Use `PgConnection::connect_with(&opts)` only.
- **`application_name` is a builder method, not a config-string parameter.** `PgConnectOptions::new().application_name("quill")` is correct. Do not jam it into the connection URL.
- **`PgSslMode` does not implement `FromStr` in a way that matches libpq spellings.** Implement the mapping manually (see `parse_ssl_mode`). The `verify-ca` / `verify-full` strings use hyphens; the enum variants use `CamelCase`. Mixing these up will silently fall through to a `prefer` default if you take a shortcut with `.unwrap_or`.
- **Integration tests are a separate crate.** Files under `src-tauri/tests/` compile as their own binaries that depend on `quill_lib`. They can only see `pub` items. That is why step 4 makes `pg`, `slots`, `registry`, `store` all `pub mod`. Forgetting the `pub` will fail to link with "unresolved import `quill_lib::pg`".
- **`DashMap` shard locks and `.await`.** Do not hold a `Ref<'_, K, V>` (returned by `DashMap::get(&k)`) across an `.await`. If a future M1.5 handler needs to use a `SlotManager` asynchronously, clone the `Arc` out of the map first, then drop the ref. The `ServerHandle: Clone` (cheap — it's just an `Arc` clone) supports this pattern.
- **`tokio::test` requires `tokio` features `macros` and `rt`.** Already present in the current `Cargo.toml` via `["rt-multi-thread", "macros", "sync", "time"]`. Nothing to add.
- **`PgConnection::connect_with` returns `Result<PgConnection, sqlx::Error>`.** `sqlx::Error::Database(err)` carries server-side messages (wrong password, missing DB). Map to `ConnectorError(e.to_string())` — do not destructure further; the string form is what M1.5 will surface to the UI.
- **`template0` is *not* connectable on stock Postgres.** It has `datallowconn = false`. The tests in this spec deliberately avoid it; the LRU test uses `budget=1` with `postgres` ↔ `template1` ping-pong instead, which exercises rule 3 on every cross-DB acquire and needs only two universally-connectable databases.
- **`SlotGuard` has no `DerefMut` until step 2 lands.** If you try to write any sqlx query against a guard before adding the `DerefMut` impl, you will get "cannot borrow data in dereference of `SlotGuard<…>` as mutable." Do step 2 before step 6.
- **`./test.sh` runs `cargo test` from `src-tauri/`.** Integration-test files in `src-tauri/tests/` are picked up automatically; no test-runner config needed.
- **No tracing/logging crate is set up.** Do not add `tracing::info!` calls. The codebase has decided silence is fine for now; M6 may revisit.

## Tests

The complete test file is in deliverable 5. Run them both ways:

```bash
# Without Postgres — every integration test silently passes after a stderr note.
./test.sh

# With Postgres — start the test container once, then run the full suite.
docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17
QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh
```

Coverage the file delivers:

1. **`connector_runs_select_one`** — `PgConnector::connect("postgres")` succeeds; `SELECT 1` returns `1`; `Connector::close` runs without panicking.
2. **`slot_manager_opens_two_distinct_databases`** — `SlotManager<PgConnector>` with `budget=2` opens connections to two distinct real databases. Verifies via `SELECT current_database()` (through `&mut *guard` — proves `DerefMut` is wired) that each guard talks to the right DB.
3. **`slot_manager_lru_evicts_with_budget_one`** — With `budget=1`, alternating between `postgres` and `template1` forces rule 3 (LRU eviction) on every acquire. Each post-eviction guard is verified via `current_database()` to confirm rebinding actually happened rather than the slot being reused stale. This is the only test in the project that exercises eviction against real sockets; the unit tests in `slots/mod.rs` only see `FakeConn` open/close counts.
4. **`bad_password_returns_connect_error`** — Wrong password returns `Err(ConnectorError(_))`. Verifies that the `map_err` chain doesn't swallow auth failures into a panic.

No new unit tests inside `pg/mod.rs` itself. Unit-level testing of `PgConnector` without a real Postgres has no value — the only behavior to verify is the round trip to a server, which is the integration tests' job. The slot manager's own correctness is already covered by `slots/mod.rs::tests`; the new `DerefMut` impl is exercised end-to-end by tests 2 and 3.

## Acceptance criteria

A reviewer must be able to tick each box by running a single command or reading one file.

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds on a machine **without** `QUILL_TEST_PG_URL` set — the four integration tests log "QUILL_TEST_PG_URL not set; skipping" to stderr and pass.
- [ ] `QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh` succeeds against a local `postgres:17` Docker container, with all four integration tests reporting `ok`.
- [ ] `grep -RIn "PgPool" src-tauri/src` returns no results. (The slot manager is the pool.)
- [ ] `grep -RIn "expose_secret\|SecretString" src-tauri/src` shows the password is wrapped in `SecretString` and only exposed inside `PgConnector::connect`.
- [ ] `grep -RIn "Debug" src-tauri/src/pg` shows no `#[derive(Debug)]` on `PgConnector` (the `SecretString` field is the reason — let the absence of `Debug` make the leak path impossible).
- [ ] `src-tauri/src/lib.rs` declares `pub mod pg;`, `pub mod registry;`, `pub mod slots;`, `pub mod store;`, and the `setup` closure calls `app.manage(registry::ServerRegistry::default())`.
- [ ] `grep -n "DerefMut" src-tauri/src/slots/mod.rs` shows exactly one impl block, the one from deliverable 1b. No other change to that file.
- [ ] No Tauri commands beyond the existing `greet` are added (M1.5 owns the command surface).
- [ ] Exactly three new files exist: `src-tauri/src/pg/mod.rs`, `src-tauri/src/registry.rs`, `src-tauri/tests/pg_integration.rs`. Existing files modified: `Cargo.toml`, `src/slots/mod.rs`, `src/lib.rs`.

## Out of scope

- Tauri commands using the registry (`connect_server`, `disconnect_server`, `run_query`, `get_slot_state`) — **M1.5**.
- Persisting passwords to the OS keychain — **M6**. `connections.password_ref` stays `NULL`; the password reaches `PgConnector` in-process from the user's input.
- Cancellation plumbing (capturing backend PID + secret, building `CancelRequest` packets) — **M3**. A `TODO(M3)` comment in `pg/mod.rs` is the only acknowledgement in this milestone.
- Any frontend code — **M1.6**.
- Multi-engine abstractions — explicitly forbidden by AGENTS.md scope rules.
