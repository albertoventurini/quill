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

use sqlx::Column;
use sqlx::Row;
use sqlx::postgres::PgRow;

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

    let ssl_mode = PgConnector::parse_ssl_mode(&conn.ssl_mode).expect("ssl_mode");
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

        let mut guard = slot_manager.acquire(&dsn.database).await.expect("acquire");

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

    let err =
        quill_lib::commands::CommandError::UnknownConnection("connection 42 not found".into());
    let json = serde_json::to_value(err).expect("serialize");
    assert_eq!(json["kind"], "UnknownConnection");
    assert_eq!(json["message"], "connection 42 not found");
}
