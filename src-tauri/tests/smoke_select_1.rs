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

use secrecy::SecretString;
use tokio_postgres::NoTls;

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
            ssl_mode: "disable".into(),
            slot_budget: 2,
            credential_source: "password".into(),
            bao_role_path: None,
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
        username: dsn.username.clone(),
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

        let guard = slot_manager.acquire(&dsn.database).await.expect("acquire");

        let rows = guard.query("SELECT 1 AS one", &[]).await.expect("SELECT 1");

        let col_name = rows[0].columns()[0].name().to_string();
        let val: i32 = rows[0].try_get::<_, i32>(0).expect("column 0 as i32");
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

// ── Test 4: pg_row_to_json maps Postgres types correctly ─────────────

#[tokio::test]
async fn pg_row_to_json_maps_int4_correctly() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let rows = client
        .query("SELECT 1 AS num", &[])
        .await
        .expect("SELECT 1");

    let values = quill_lib::commands::pg_row_to_json(&rows[0]);
    assert_eq!(values.len(), 1, "should have 1 column");
    match &values[0] {
        serde_json::Value::Number(n) => assert_eq!(n.as_i64(), Some(1), "SELECT 1 should be 1"),
        other => panic!("expected Number(1), got {other:?}"),
    }
}

#[tokio::test]
async fn pg_row_to_json_maps_bool_correctly() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let rows = client
        .query("SELECT true AS flag", &[])
        .await
        .expect("SELECT true");

    let values = quill_lib::commands::pg_row_to_json(&rows[0]);
    assert_eq!(values.len(), 1, "should have 1 column");
    match &values[0] {
        serde_json::Value::Bool(b) => assert!(*b, "SELECT true should be true"),
        other => panic!("expected Bool(true), got {other:?}"),
    }
}

#[tokio::test]
#[allow(clippy::approx_constant)]
async fn pg_row_to_json_maps_float_correctly() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let rows = client
        .query("SELECT 3.14::float8 AS val", &[])
        .await
        .expect("SELECT 3.14::float8");

    let values = quill_lib::commands::pg_row_to_json(&rows[0]);
    assert_eq!(values.len(), 1, "should have 1 column");
    match &values[0] {
        serde_json::Value::Number(n) => {
            let f = n.as_f64().expect("should be f64");
            assert!(
                (f - 3.14).abs() < 0.001,
                "SELECT 3.14 should be ~3.14, got {f}"
            );
        }
        other => panic!("expected Number, got {other:?}"),
    }
}

#[tokio::test]
async fn pg_row_to_json_maps_text_correctly() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let rows = client
        .query("SELECT 'hello'::text AS msg", &[])
        .await
        .expect("SELECT 'hello'::text");

    let values = quill_lib::commands::pg_row_to_json(&rows[0]);
    assert_eq!(values.len(), 1, "should have 1 column");
    match &values[0] {
        serde_json::Value::String(s) => assert_eq!(s, "hello"),
        other => panic!("expected String(\"hello\"), got {other:?}"),
    }
}

#[tokio::test]
async fn pg_row_to_json_maps_enum_correctly() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    // Idempotent: skip the CREATE TYPE if the test has already been run
    // against this database.
    client
        .batch_execute(
            "DO $$ BEGIN
                CREATE TYPE quill_test_mood AS ENUM ('sad', 'ok', 'happy');
            EXCEPTION WHEN duplicate_object THEN NULL;
            END $$;",
        )
        .await
        .expect("create enum type");

    let rows = client
        .query("SELECT 'happy'::quill_test_mood AS m", &[])
        .await
        .expect("SELECT enum");

    let values = quill_lib::commands::pg_row_to_json(&rows[0]);
    assert_eq!(values.len(), 1, "should have 1 column");
    match &values[0] {
        serde_json::Value::String(s) => assert_eq!(s, "happy"),
        other => panic!("expected String(\"happy\"), got {other:?}"),
    }
}

#[tokio::test]
async fn pg_row_to_json_maps_int_array_correctly() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    // Include a NULL element to exercise the per-element Option decoding.
    let rows = client
        .query("SELECT ARRAY[1, NULL, 3]::int[] AS xs", &[])
        .await
        .expect("SELECT int[]");

    let values = quill_lib::commands::pg_row_to_json(&rows[0]);
    assert_eq!(values.len(), 1, "should have 1 column");
    assert_eq!(
        values[0],
        serde_json::json!([1, serde_json::Value::Null, 3]),
        "got {:?}",
        values[0]
    );
}

#[tokio::test]
async fn pg_row_to_json_maps_text_array_correctly() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let rows = client
        .query("SELECT ARRAY['a', 'b']::text[] AS xs", &[])
        .await
        .expect("SELECT text[]");

    let values = quill_lib::commands::pg_row_to_json(&rows[0]);
    assert_eq!(values.len(), 1, "should have 1 column");
    assert_eq!(
        values[0],
        serde_json::json!(["a", "b"]),
        "got {:?}",
        values[0]
    );
}

#[tokio::test]
async fn pg_row_to_json_maps_enum_array_correctly() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    // Reuses the enum type created by `pg_row_to_json_maps_enum_correctly`;
    // idempotent CREATE so this test passes regardless of run order.
    client
        .batch_execute(
            "DO $$ BEGIN
                CREATE TYPE quill_test_mood AS ENUM ('sad', 'ok', 'happy');
            EXCEPTION WHEN duplicate_object THEN NULL;
            END $$;",
        )
        .await
        .expect("create enum type");

    let rows = client
        .query("SELECT ARRAY['happy', 'sad']::quill_test_mood[] AS xs", &[])
        .await
        .expect("SELECT enum[]");

    let values = quill_lib::commands::pg_row_to_json(&rows[0]);
    assert_eq!(values.len(), 1, "should have 1 column");
    assert_eq!(
        values[0],
        serde_json::json!(["happy", "sad"]),
        "got {:?}",
        values[0]
    );
}

#[tokio::test]
async fn pg_row_to_json_marks_unsupported_type() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    // `point` has no FromSql decode here, so it must surface as the honest
    // unsupported marker rather than a misleading NULL.
    let rows = client
        .query("SELECT '(1,2)'::point AS pt", &[])
        .await
        .expect("SELECT point");

    let values = quill_lib::commands::pg_row_to_json(&rows[0]);
    assert_eq!(values.len(), 1, "should have 1 column");
    assert_eq!(
        values[0],
        serde_json::json!({ "__quill_unsupported__": "point" }),
        "got {:?}",
        values[0]
    );
}

#[tokio::test]
async fn pg_row_to_json_maps_null_correctly() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let rows = client
        .query("SELECT NULL::int4 AS nothing", &[])
        .await
        .expect("SELECT NULL::int4");

    let values = quill_lib::commands::pg_row_to_json(&rows[0]);
    assert_eq!(values.len(), 1, "should have 1 column");
    assert!(
        values[0].is_null(),
        "NULL should remain null, got {:?}",
        values[0]
    );
}

// ── Test 5: bare SELECT yields an empty result from Postgres ─────────────
// (Postgres treats a bare SELECT with no column list as a single empty-row
// result.  Quill's `run_query` command adds its own validation above sqlx;
// that validation is tested in the unit-tests in commands/mod.rs.)

#[tokio::test]
async fn bare_select_returns_empty_row_from_postgres() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        dsn.username, dsn.password, dsn.host, dsn.port, dsn.database
    );
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let result = client.query("SELECT ", &[]).await;

    // Postgres treats bare SELECT as a valid query returning one empty row.
    let rows = result.expect("bare SELECT should succeed at the PG level");
    assert_eq!(rows.len(), 1);
}
