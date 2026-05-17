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
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

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
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let mgr = SlotManager::new(connector_from(&dsn), 2);

    let mut g1 = mgr.acquire("postgres").await.expect("acquire postgres");
    let mut g2 = mgr.acquire("template1").await.expect("acquire template1");

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
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };

    let mgr = SlotManager::new(connector_from(&dsn), 1);

    let g = mgr.acquire("postgres").await.expect("acquire postgres");
    drop(g);

    let mut g = mgr.acquire("template1").await.expect("acquire template1");
    let (db,): (String,) = sqlx::query_as("SELECT current_database()")
        .fetch_one(&mut *g)
        .await
        .expect("current_database after eviction");
    assert_eq!(db, "template1");
    drop(g);

    let mut g = mgr
        .acquire("postgres")
        .await
        .expect("acquire postgres again");
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
    let Some(mut dsn) = dsn() else {
        skip_note();
        return;
    };
    dsn.password.push_str("-wrong");

    let connector = connector_from(&dsn);
    let result = connector.connect(&dsn.database).await;
    assert!(result.is_err(), "expected auth failure, got Ok");
}
