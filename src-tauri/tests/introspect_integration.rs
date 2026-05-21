//! Integration tests for `introspect` against a real Postgres.
//!
//! Run with:
//!   QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh
//!
//! Without the env var, every test in this file silently passes after a
//! one-line note on stderr.

use secrecy::SecretString;

use quill_lib::introspect::{
    self, DatabaseInfo, FunctionKind, PAYLOAD_VERSION, RelationKind, SchemaPayload,
};
use quill_lib::pg::{PgConnector, SslPolicy};
use quill_lib::slots::Connector;

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
    eprintln!("QUILL_TEST_PG_URL not set; skipping introspect_integration test");
}

fn connector_from(dsn: &TestDsn) -> PgConnector {
    PgConnector {
        host: dsn.host.clone(),
        port: dsn.port,
        username: dsn.username.clone(),
        password: SecretString::from(dsn.password.clone()),
        ssl_mode: SslPolicy::Disable,
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_databases_returns_postgres_and_excludes_template0() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
    let connector = connector_from(&dsn);
    let conn = connector.connect(&dsn.database).await.expect("connect");

    let dbs = introspect::list_databases(&conn)
        .await
        .expect("list_databases");
    let names: Vec<&str> = dbs.iter().map(|d: &DatabaseInfo| d.name.as_str()).collect();

    assert!(
        names.contains(&"postgres"),
        "postgres should be listed; got {names:?}"
    );
    assert!(
        !names.contains(&"template0"),
        "template0 has datallowconn=false and must be excluded; got {names:?}"
    );
    assert!(
        !names.contains(&"template1"),
        "template1 is datistemplate=true and must be excluded; got {names:?}"
    );

    // Sorted.
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "list_databases must return sorted names");

    PgConnector::close(conn).await;
}

#[tokio::test]
async fn introspect_database_returns_public_schema_with_v1_payload() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
    let connector = connector_from(&dsn);
    let conn = connector.connect(&dsn.database).await.expect("connect");

    let payload: SchemaPayload = introspect::introspect_database(&conn)
        .await
        .expect("introspect_database");

    assert_eq!(payload.v, PAYLOAD_VERSION);
    let public = payload
        .schemas
        .iter()
        .find(|s| s.name == "public")
        .expect("public schema must exist on a stock postgres database");
    // No `pg_catalog`, no `information_schema`, no `pg_toast` in the output.
    for s in &payload.schemas {
        assert!(
            !s.name.starts_with("pg_") && s.name != "information_schema",
            "system schema leaked into introspection: {}",
            s.name
        );
    }
    let _ = public; // existence is the assertion

    PgConnector::close(conn).await;
}

#[tokio::test]
async fn introspect_database_distinguishes_table_view_matview_function() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
    let connector = connector_from(&dsn);
    let conn = connector.connect(&dsn.database).await.expect("connect");

    // Build a small fixture inside a transient schema so we don't pollute
    // the connecting user's `public`.  ROLLBACK at the end discards it.
    //
    // We use the same connection so the introspection call sees the
    // uncommitted DDL inside its own transaction.
    conn.execute("BEGIN", &[]).await.expect("begin");
    conn.execute("CREATE SCHEMA quill_m22_fixture", &[])
        .await
        .expect("create schema");
    conn.execute("CREATE TABLE quill_m22_fixture.t1 (id int)", &[])
        .await
        .expect("create table");
    conn.execute("CREATE VIEW quill_m22_fixture.v1 AS SELECT 1 AS x", &[])
        .await
        .expect("create view");
    conn.execute(
        "CREATE MATERIALIZED VIEW quill_m22_fixture.m1 AS SELECT 1 AS x",
        &[],
    )
    .await
    .expect("create matview");
    conn.execute(
        "CREATE FUNCTION quill_m22_fixture.f1() RETURNS int LANGUAGE sql AS 'SELECT 1'",
        &[],
    )
    .await
    .expect("create function");
    conn.execute(
        "CREATE PROCEDURE quill_m22_fixture.p1() LANGUAGE sql AS ''",
        &[],
    )
    .await
    .expect("create procedure");

    let payload = introspect::introspect_database(&conn)
        .await
        .expect("introspect_database");

    let schema = payload
        .schemas
        .iter()
        .find(|s| s.name == "quill_m22_fixture")
        .expect("fixture schema must appear");

    let by_name = |name: &str| schema.relations.iter().find(|r| r.name == name);
    assert_eq!(by_name("t1").map(|r| r.kind), Some(RelationKind::Table));
    assert_eq!(by_name("v1").map(|r| r.kind), Some(RelationKind::View));
    assert_eq!(by_name("m1").map(|r| r.kind), Some(RelationKind::Matview));

    let fns_by_name = |name: &str| schema.functions.iter().find(|f| f.name == name);
    assert_eq!(
        fns_by_name("f1").map(|f| f.kind),
        Some(FunctionKind::Function)
    );
    assert_eq!(
        fns_by_name("p1").map(|f| f.kind),
        Some(FunctionKind::Procedure)
    );

    conn.execute("ROLLBACK", &[]).await.expect("rollback");
    PgConnector::close(conn).await;
}

#[tokio::test]
async fn introspect_database_serializes_to_json_string() {
    // Round-trip through the same path commands::set_schema_cache will take.
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
    let connector = connector_from(&dsn);
    let conn = connector.connect(&dsn.database).await.expect("connect");

    let payload = introspect::introspect_database(&conn)
        .await
        .expect("introspect_database");
    let json = serde_json::to_string(&payload).expect("serialize");
    let back: SchemaPayload = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(payload.v, back.v);
    assert_eq!(payload.schemas, back.schemas);

    PgConnector::close(conn).await;
}
