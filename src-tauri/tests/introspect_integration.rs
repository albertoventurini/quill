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
    let (conn, _cancel) = connector.connect(&dsn.database).await.expect("connect");

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
    let (conn, _cancel) = connector.connect(&dsn.database).await.expect("connect");

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
    let (conn, _cancel) = connector.connect(&dsn.database).await.expect("connect");

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
    let (conn, _cancel) = connector.connect(&dsn.database).await.expect("connect");

    let payload = introspect::introspect_database(&conn)
        .await
        .expect("introspect_database");
    let json = serde_json::to_string(&payload).expect("serialize");
    let back: SchemaPayload = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(payload.v, back.v);
    assert_eq!(payload.schemas, back.schemas);

    PgConnector::close(conn).await;
}

#[tokio::test]
async fn introspect_database_returns_columns_for_relations() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
    let connector = connector_from(&dsn);
    let (client, _cancel) = connector.connect(&dsn.database).await.expect("connect");

    client.batch_execute("BEGIN").await.expect("begin");
    client
        .batch_execute("CREATE SCHEMA quill_m41_fixture")
        .await
        .expect("create schema");
    client
        .batch_execute(
            "CREATE TABLE quill_m41_fixture.users (
             id           integer NOT NULL,
             email        text,
             signup_at    timestamp with time zone NOT NULL
         )",
        )
        .await
        .expect("create table");
    // A view with two projected columns to confirm views also report columns.
    client
        .batch_execute(
            "CREATE VIEW quill_m41_fixture.user_emails AS
             SELECT id, email FROM quill_m41_fixture.users",
        )
        .await
        .expect("create view");

    let payload = introspect::introspect_database(&client)
        .await
        .expect("introspect_database");

    assert_eq!(payload.v, 3, "ERD-era payload version must be 3");

    let schema = payload
        .schemas
        .iter()
        .find(|s| s.name == "quill_m41_fixture")
        .expect("fixture schema must appear");

    let users = schema
        .relations
        .iter()
        .find(|r| r.name == "users")
        .expect("users table must appear");

    assert_eq!(
        users.columns.len(),
        3,
        "users has three columns; got {:?}",
        users.columns
    );

    // Columns must be in attnum order.
    assert_eq!(users.columns[0].name, "id");
    assert_eq!(users.columns[0].type_name, "integer");
    assert!(users.columns[0].not_null, "id is NOT NULL");
    assert_eq!(users.columns[0].position, 1);

    assert_eq!(users.columns[1].name, "email");
    assert_eq!(users.columns[1].type_name, "text");
    assert!(!users.columns[1].not_null, "email is nullable");
    assert_eq!(users.columns[1].position, 2);

    assert_eq!(users.columns[2].name, "signup_at");
    assert_eq!(users.columns[2].type_name, "timestamp with time zone");
    assert!(users.columns[2].not_null);
    assert_eq!(users.columns[2].position, 3);

    let view = schema
        .relations
        .iter()
        .find(|r| r.name == "user_emails")
        .expect("user_emails view must appear");
    assert_eq!(
        view.columns.len(),
        2,
        "view reports projected columns; got {:?}",
        view.columns
    );
    assert_eq!(view.columns[0].name, "id");
    assert_eq!(view.columns[1].name, "email");

    client.batch_execute("ROLLBACK").await.expect("rollback");
}

#[tokio::test]
async fn introspect_database_captures_primary_and_foreign_keys() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
    let connector = connector_from(&dsn);
    let (client, _cancel) = connector.connect(&dsn.database).await.expect("connect");

    client.batch_execute("BEGIN").await.expect("begin");
    client
        .batch_execute("CREATE SCHEMA quill_erd_fixture")
        .await
        .expect("create schema");
    // Composite primary key on the parent.
    client
        .batch_execute(
            "CREATE TABLE quill_erd_fixture.customers (
                 tenant_id integer NOT NULL,
                 id        integer NOT NULL,
                 name      text,
                 PRIMARY KEY (tenant_id, id)
             )",
        )
        .await
        .expect("create customers");
    // Composite foreign key referencing the parent's composite PK.
    client
        .batch_execute(
            "CREATE TABLE quill_erd_fixture.orders (
                 id          integer PRIMARY KEY,
                 tenant_id   integer NOT NULL,
                 customer_id integer NOT NULL,
                 CONSTRAINT orders_customer_fkey
                   FOREIGN KEY (tenant_id, customer_id)
                   REFERENCES quill_erd_fixture.customers (tenant_id, id)
             )",
        )
        .await
        .expect("create orders");

    let payload = introspect::introspect_database(&client)
        .await
        .expect("introspect_database");

    let schema = payload
        .schemas
        .iter()
        .find(|s| s.name == "quill_erd_fixture")
        .expect("fixture schema must appear");

    // Composite PK flags both key columns and nothing else.
    let customers = schema
        .relations
        .iter()
        .find(|r| r.name == "customers")
        .expect("customers table must appear");
    let pk_cols: Vec<&str> = customers
        .columns
        .iter()
        .filter(|c| c.is_primary_key)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(pk_cols, vec!["tenant_id", "id"], "composite PK columns");
    assert!(
        customers.foreign_keys.is_empty(),
        "customers has no outgoing FKs"
    );

    // Composite FK keeps column order and points at the parent's PK columns.
    let orders = schema
        .relations
        .iter()
        .find(|r| r.name == "orders")
        .expect("orders table must appear");
    assert_eq!(orders.foreign_keys.len(), 1, "one FK on orders");
    let fk = &orders.foreign_keys[0];
    assert_eq!(fk.columns, vec!["tenant_id", "customer_id"]);
    assert_eq!(fk.referenced_schema, "quill_erd_fixture");
    assert_eq!(fk.referenced_table, "customers");
    assert_eq!(fk.referenced_columns, vec!["tenant_id", "id"]);
    // `orders.id` is the singleton PK here.
    let orders_pk: Vec<&str> = orders
        .columns
        .iter()
        .filter(|c| c.is_primary_key)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(orders_pk, vec!["id"]);

    client.batch_execute("ROLLBACK").await.expect("rollback");
    PgConnector::close(client).await;
}

#[tokio::test]
async fn introspect_database_captures_default_search_path() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
    let connector = connector_from(&dsn);
    let (client, _cancel) = connector.connect(&dsn.database).await.expect("connect");

    let payload = introspect::introspect_database(&client)
        .await
        .expect("introspect_database");

    assert!(
        payload.search_path.iter().any(|s| s == "public"),
        "default search_path should include `public`; got {:?}",
        payload.search_path,
    );
    assert!(
        !payload.search_path.iter().any(|s| s == "pg_catalog"),
        "pg_catalog must be excluded from search_path; got {:?}",
        payload.search_path,
    );

    PgConnector::close(client).await;
}

#[tokio::test]
async fn introspect_database_resolves_dollar_user_in_search_path() {
    let Some(dsn) = dsn() else {
        skip_note();
        return;
    };
    let connector = connector_from(&dsn);
    let (client, _cancel) = connector.connect(&dsn.database).await.expect("connect");

    client.batch_execute("BEGIN").await.expect("begin");
    client
        .batch_execute(&format!(
            "CREATE SCHEMA \"{}\"",
            dsn.username.replace('"', "\"\"")
        ))
        .await
        .expect("create user schema");
    client
        .batch_execute("SET search_path = \"$user\", public")
        .await
        .expect("set search_path");

    let payload = introspect::introspect_database(&client)
        .await
        .expect("introspect_database");

    assert_eq!(
        payload.search_path,
        vec![dsn.username.clone(), "public".to_string()],
        "search_path must have $user resolved to {} and exclude pg_catalog",
        dsn.username,
    );

    client.batch_execute("ROLLBACK").await.expect("rollback");

    PgConnector::close(client).await;
}
