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
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrations");

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
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
    let (_pool, registry, server_id) = fresh_pool_and_server(&dsn).await;

    let handle = registry.by_id.get(&server_id).expect("handle");
    let mgr = handle.slot_manager.clone();
    drop(handle);

    let mut guard = mgr.acquire(&dsn.database).await.expect("acquire");
    let dbs = introspect::list_databases(&mut guard)
        .await
        .expect("list_databases");
    drop(guard);

    let names: Vec<&str> = dbs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"postgres"), "got {names:?}");

    mgr.disconnect_all().await;
}

#[tokio::test]
async fn ensure_payload_misses_then_hits() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
    let (pool, registry, server_id) = fresh_pool_and_server(&dsn).await;

    // Cache is empty.
    assert!(
        store::get_schema_cache(&pool, server_id, &dsn.database)
            .await
            .unwrap()
            .is_none()
    );

    // ── Cache-miss path: replicate `ensure_payload` inline ────────────
    let handle = registry.by_id.get(&server_id).expect("handle");
    let mgr = handle.slot_manager.clone();
    drop(handle);

    let mut guard = mgr.acquire(&dsn.database).await.expect("acquire");
    let payload1 = introspect::introspect_database(&mut guard)
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
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
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
    let fresh = introspect::introspect_database(&mut guard)
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
