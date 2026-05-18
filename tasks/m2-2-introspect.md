# M2.2 — `introspect` module + Postgres integration tests

## Goal

**Before:** The backend can connect to a Postgres server and run arbitrary user SQL (M1.5's `run_query`), but it has no understanding of the server's schema. Nothing in the crate knows how to list databases, schemas, tables, views, materialized views, or functions. The `schema_cache` table from M2.1 exists but is empty and unreferenced — the value type `store::SchemaCacheRow` holds an opaque `payload_json: String` with no canonical shape.

**After:** A new module `src-tauri/src/introspect/mod.rs` exposes two public async functions: `list_databases(conn)` for the server-wide database list, and `introspect_database(conn)` which fetches every schema, relation, and function for the *currently-connected database* and returns a strongly-typed, versioned `SchemaPayload`. The payload's wire shape is locked at v1 and documented as the canonical contents of `schema_cache.payload_json`. The catalog queries are issued against `pg_database`, `pg_namespace`, `pg_class`, and `pg_proc` — no `psql`-style shell-outs, no `information_schema`. The module is **headless**: it takes a `&mut PgConnection` (which the slot manager hands out as `&mut *guard`) and does no cache I/O of its own. Integration tests (gated on `QUILL_TEST_PG_URL`) prove every query runs against a real Postgres and returns the expected canonical objects (`postgres`, `template1`, the `public` schema, etc.). M2.3 will wire these into Tauri commands; M2.4 will turn the result into a tree.

## Current state

Every file below already exists. Read in full before writing anything; the connector trait, the existing PG integration test pattern, and the M2.1 store shape together fix the integration points.

### `src-tauri/Cargo.toml`

```toml
[package]
name = "quill"
version = "0.1.0"
description = "A Tauri App"
authors = ["you"]
edition = "2024"

[lib]
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
base64 = "0.22"

[dev-dependencies]
url = "2"
```

No additions needed for this task — all introspection queries use `sqlx::query_as::<_, (T,)>` or `sqlx::FromRow`, both already in scope.

### `src-tauri/src/lib.rs`

Currently:

```rust
pub mod commands;
pub mod pg;
pub mod registry;
pub mod slots;
pub mod store;

// ... setup + invoke_handler ...
```

This file gains exactly one line: `pub mod introspect;`. The integration tests in this task need it `pub` (they live in a separate test crate that can only see `pub` items). Do not add Tauri commands here — M2.3 owns the command surface.

### `src-tauri/src/pg/mod.rs`

The connector returns `sqlx::PgConnection` from `Connector::connect`. The slot manager hands it out via `SlotGuard<'_, PgConnector>` with `DerefMut<Target = PgConnection>`. Inside `introspect`, the input type is plain `&mut sqlx::PgConnection` — no dependency on `pg::PgConnector` or `slots::SlotGuard`. This is intentional: introspection works on any connection and stays trivially testable without the registry.

### `src-tauri/src/slots/mod.rs`

Has `impl<C: Connector> DerefMut for SlotGuard<'_, C>` (line ~158) so callers can pass `&mut *guard` to sqlx executors. Used by `commands::run_query`; will be used by M2.3 in the same idiom.

### `src-tauri/src/store/mod.rs` (post-M2.1)

After M2.1 lands, `SchemaCacheRow.payload_json` is the on-disk JSON blob produced by this task. The blob's canonical shape is the `SchemaPayload` struct defined here.

### `src-tauri/tests/pg_integration.rs`

Existing 147-line file from M1.4. Read the `dsn()`, `skip_note()`, and `connector_from()` helpers — the new test file in this task reuses the same pattern verbatim. The helpers are not extracted into a shared module because there's no `tests/common` infrastructure set up; copying the helpers across files is the existing convention.

```rust
struct TestDsn { host: String, port: u16, username: String, password: String, database: String }

fn dsn() -> Option<TestDsn> {
    let raw = std::env::var("QUILL_TEST_PG_URL").ok()?;
    let u = url::Url::parse(&raw).expect("QUILL_TEST_PG_URL must be a valid postgres URL");
    Some(TestDsn { /* fields */ })
}

fn skip_note() { eprintln!("QUILL_TEST_PG_URL not set; skipping pg_integration test"); }
```

### `src-tauri/tests/smoke_select_1.rs`

Also from M1.5. Reuses the same `dsn()` / `skip_note()` shape. Confirms the pattern.

## Postgres system catalogs — the four queries

Read this section before writing the module. These four queries are the entire surface of M2.2's Postgres interaction. Every column maps directly to a field in `SchemaPayload` (defined in deliverable 2).

**Why `pg_catalog`, not `information_schema`:** `information_schema` is SQL-standard but always loses the Postgres-specific information Quill needs (`relkind` distinguishing tables from matviews; `prokind` distinguishing functions from procedures from aggregates). Querying `pg_catalog` directly is the canonical Postgres approach and is what `psql`'s `\d*` commands do under the hood. `PRD.md` §11 spells out this choice.

### Query 1 — `list_databases`

```sql
SELECT datname
FROM pg_database
WHERE datallowconn AND NOT datistemplate
ORDER BY datname
```

- `datallowconn = false` excludes `template0` (which rejects connections).
- `datistemplate = true` excludes `template0`/`template1` and any user-created templates from the visible list. (`template1` *is* connectable but the user never browses it as a working DB.)
- Returns: `Vec<DatabaseInfo>` where `DatabaseInfo { name: String }`.

### Query 2 — schemas in the connected database

```sql
SELECT nspname
FROM pg_namespace
WHERE nspname NOT LIKE 'pg\_%' ESCAPE '\'
  AND nspname <> 'information_schema'
ORDER BY nspname
```

- Excludes `pg_catalog`, `pg_toast`, `pg_temp_*`, `pg_toast_temp_*`, and `information_schema`.
- The `\` ESCAPE clause is required because `_` is a wildcard in `LIKE`; without it, `pg_%` matches anything starting with a single char and `%`.
- Returns: `Vec<String>` (schema names; richer metadata can come later).

### Query 3 — relations (tables/views/matviews/partitioned) in all visible schemas

```sql
SELECT n.nspname AS schema,
       c.relname AS name,
       c.relkind AS kind
FROM pg_class c
JOIN pg_namespace n ON c.relnamespace = n.oid
WHERE c.relkind IN ('r', 'v', 'm', 'p')
  AND n.nspname NOT LIKE 'pg\_%' ESCAPE '\'
  AND n.nspname <> 'information_schema'
ORDER BY n.nspname, c.relname
```

- `relkind`: `r` = ordinary table, `v` = view, `m` = materialized view, `p` = partitioned table.
- Other `relkind` values (`i` index, `S` sequence, `t` TOAST, `c` composite type, `I` partitioned index, `f` foreign table) are excluded — they aren't in scope for v1. Foreign tables (`f`) might be re-added in v1.1 if a user asks; tracked as a future deferral, not a v1 item.
- Returns: `Vec<(String /*schema*/, RelationInfo)>` — joined by schema in `introspect_database`.

### Query 4 — functions, procedures, aggregates, windows in all visible schemas

```sql
SELECT n.nspname AS schema,
       p.proname AS name,
       p.prokind AS kind
FROM pg_proc p
JOIN pg_namespace n ON p.pronamespace = n.oid
WHERE n.nspname NOT LIKE 'pg\_%' ESCAPE '\'
  AND n.nspname <> 'information_schema'
ORDER BY n.nspname, p.proname
```

- `prokind`: `f` = function, `p` = procedure, `a` = aggregate, `w` = window. All four are returned and tagged; the UI decides how to icon them.
- **Overloads are not deduplicated.** Postgres allows the same `proname` with different argument signatures in the same schema. v1 returns each row separately; the tree may show duplicates. This matches DBeaver/pgAdmin behaviour and is explicitly accepted in `MILESTONES.md`. Future arg-signature columns can land alongside M4's autocomplete.
- Returns: `Vec<(String /*schema*/, FunctionInfo)>`.

## Deliverables

### 1. `src-tauri/src/lib.rs` — declare the module

Add one line alongside the existing module declarations (alphabetic order):

```rust
pub mod commands;
pub mod introspect;   // <-- new
pub mod pg;
pub mod registry;
pub mod slots;
pub mod store;
```

No other changes to `lib.rs`. Verify `( cd src-tauri && cargo build )` after step 2.

### 2. `src-tauri/src/introspect/mod.rs` — new file

Create the file with the content below. The full module is ~210 lines.

```rust
//! Postgres schema introspection.
//!
//! Reads `pg_database`, `pg_namespace`, `pg_class`, and `pg_proc` directly.
//! No `information_schema`; no `psql` shell-outs.
//!
//! All functions take a borrowed `&mut PgConnection` so callers can pass
//! `&mut *guard` from a `SlotGuard<'_, PgConnector>`.  The introspection
//! module deliberately knows nothing about the slot manager or the local
//! SQLite cache — those compositional concerns belong in `commands` (M2.3).
//!
//! The output of `introspect_database` is the canonical contents of
//! `store::SchemaCacheRow.payload_json` and is versioned via the
//! `PAYLOAD_VERSION` constant.  Bumping the version is M4's problem (it
//! will add column metadata); for M2 v1 is fixed at 1.

use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use sqlx::Row;
use thiserror::Error;

/// Canonical version of the schema-cache payload.  Bumped only when the
/// `SchemaPayload` wire shape changes in a way old payloads cannot satisfy.
pub const PAYLOAD_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum IntrospectError {
    #[error("Postgres error: {0}")]
    Pg(#[from] sqlx::Error),

    #[error("unknown relkind '{0}' returned by pg_class")]
    UnknownRelKind(String),

    #[error("unknown prokind '{0}' returned by pg_proc")]
    UnknownProKind(String),
}

// ---------------------------------------------------------------------------
// Value types — the canonical wire shape of payload_json
// ---------------------------------------------------------------------------

/// A single database visible on the connected server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseInfo {
    pub name: String,
}

/// Top-level payload — one of these per `(server_id, database)` cache row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaPayload {
    /// Wire-shape version.  Matches `PAYLOAD_VERSION` at write time.
    pub v: u32,
    pub schemas: Vec<SchemaInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaInfo {
    pub name: String,
    pub relations: Vec<RelationInfo>,
    pub functions: Vec<FunctionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationInfo {
    pub name: String,
    pub kind: RelationKind,
}

/// Mirrors `pg_class.relkind` for the four kinds Quill exposes in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Table,
    View,
    Matview,
    PartitionedTable,
}

impl RelationKind {
    fn from_pg(c: char) -> Result<Self, IntrospectError> {
        match c {
            'r' => Ok(Self::Table),
            'v' => Ok(Self::View),
            'm' => Ok(Self::Matview),
            'p' => Ok(Self::PartitionedTable),
            other => Err(IntrospectError::UnknownRelKind(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionInfo {
    pub name: String,
    pub kind: FunctionKind,
}

/// Mirrors `pg_proc.prokind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionKind {
    Function,
    Procedure,
    Aggregate,
    Window,
}

impl FunctionKind {
    fn from_pg(c: char) -> Result<Self, IntrospectError> {
        match c {
            'f' => Ok(Self::Function),
            'p' => Ok(Self::Procedure),
            'a' => Ok(Self::Aggregate),
            'w' => Ok(Self::Window),
            other => Err(IntrospectError::UnknownProKind(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// List every connectable, non-template database on the server.
///
/// Ordered by `datname`.  Excludes `template0` (datallowconn=false) and
/// every datistemplate=true row including `template1`.
pub async fn list_databases(
    conn: &mut PgConnection,
) -> Result<Vec<DatabaseInfo>, IntrospectError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT datname \
         FROM pg_database \
         WHERE datallowconn AND NOT datistemplate \
         ORDER BY datname",
    )
    .fetch_all(conn)
    .await?;

    Ok(rows.into_iter().map(|(name,)| DatabaseInfo { name }).collect())
}

/// Fetch the full schema payload for the currently-connected database.
///
/// Issues three independent SQL queries serially over the same connection
/// (schemas, relations, functions) and stitches them into a single
/// `SchemaPayload`.  Schemas with no relations and no functions still
/// appear (the user expects to see an empty schema as an empty folder).
pub async fn introspect_database(
    conn: &mut PgConnection,
) -> Result<SchemaPayload, IntrospectError> {
    let schemas = list_schema_names(&mut *conn).await?;
    let relations = list_all_relations(&mut *conn).await?;
    let functions = list_all_functions(&mut *conn).await?;

    let mut by_schema: std::collections::BTreeMap<String, SchemaInfo> = schemas
        .into_iter()
        .map(|name| (name.clone(), SchemaInfo { name, relations: Vec::new(), functions: Vec::new() }))
        .collect();

    for (schema, rel) in relations {
        by_schema
            .entry(schema.clone())
            .or_insert_with(|| SchemaInfo { name: schema, relations: Vec::new(), functions: Vec::new() })
            .relations
            .push(rel);
    }
    for (schema, func) in functions {
        by_schema
            .entry(schema.clone())
            .or_insert_with(|| SchemaInfo { name: schema, relations: Vec::new(), functions: Vec::new() })
            .functions
            .push(func);
    }

    Ok(SchemaPayload {
        v: PAYLOAD_VERSION,
        schemas: by_schema.into_values().collect(),
    })
}

// ---------------------------------------------------------------------------
// Internals — one query per system catalog
// ---------------------------------------------------------------------------

async fn list_schema_names(
    conn: &mut PgConnection,
) -> Result<Vec<String>, IntrospectError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        r"SELECT nspname
          FROM pg_namespace
          WHERE nspname NOT LIKE 'pg\_%' ESCAPE '\'
            AND nspname <> 'information_schema'
          ORDER BY nspname",
    )
    .fetch_all(conn)
    .await?;

    Ok(rows.into_iter().map(|(s,)| s).collect())
}

async fn list_all_relations(
    conn: &mut PgConnection,
) -> Result<Vec<(String, RelationInfo)>, IntrospectError> {
    let rows = sqlx::query(
        r"SELECT n.nspname AS schema,
                 c.relname AS name,
                 c.relkind::text AS kind
          FROM pg_class c
          JOIN pg_namespace n ON c.relnamespace = n.oid
          WHERE c.relkind IN ('r', 'v', 'm', 'p')
            AND n.nspname NOT LIKE 'pg\_%' ESCAPE '\'
            AND n.nspname <> 'information_schema'
          ORDER BY n.nspname, c.relname",
    )
    .fetch_all(conn)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let schema: String = row.try_get("schema")?;
        let name: String = row.try_get("name")?;
        let kind_str: String = row.try_get("kind")?;
        let kind_char = kind_str.chars().next().ok_or_else(|| {
            IntrospectError::UnknownRelKind(String::from(""))
        })?;
        let kind = RelationKind::from_pg(kind_char)?;
        out.push((schema, RelationInfo { name, kind }));
    }
    Ok(out)
}

async fn list_all_functions(
    conn: &mut PgConnection,
) -> Result<Vec<(String, FunctionInfo)>, IntrospectError> {
    let rows = sqlx::query(
        r"SELECT n.nspname AS schema,
                 p.proname AS name,
                 p.prokind::text AS kind
          FROM pg_proc p
          JOIN pg_namespace n ON p.pronamespace = n.oid
          WHERE n.nspname NOT LIKE 'pg\_%' ESCAPE '\'
            AND n.nspname <> 'information_schema'
          ORDER BY n.nspname, p.proname",
    )
    .fetch_all(conn)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let schema: String = row.try_get("schema")?;
        let name: String = row.try_get("name")?;
        let kind_str: String = row.try_get("kind")?;
        let kind_char = kind_str.chars().next().ok_or_else(|| {
            IntrospectError::UnknownProKind(String::from(""))
        })?;
        let kind = FunctionKind::from_pg(kind_char)?;
        out.push((schema, FunctionInfo { name, kind }));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests (payload (de)serialization only — DB tests live in tests/)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_through_json() {
        let payload = SchemaPayload {
            v: PAYLOAD_VERSION,
            schemas: vec![SchemaInfo {
                name: "public".into(),
                relations: vec![
                    RelationInfo { name: "users".into(), kind: RelationKind::Table },
                    RelationInfo { name: "user_emails".into(), kind: RelationKind::View },
                ],
                functions: vec![FunctionInfo {
                    name: "uuid_generate_v4".into(),
                    kind: FunctionKind::Function,
                }],
            }],
        };

        let s = serde_json::to_string(&payload).expect("serialize");
        let back: SchemaPayload = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(payload, back);
    }

    #[test]
    fn relation_kind_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_value(RelationKind::PartitionedTable).unwrap(),
            serde_json::json!("partitioned_table")
        );
        assert_eq!(
            serde_json::to_value(RelationKind::Matview).unwrap(),
            serde_json::json!("matview")
        );
    }

    #[test]
    fn function_kind_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_value(FunctionKind::Window).unwrap(),
            serde_json::json!("window")
        );
    }

    #[test]
    fn relkind_from_pg_rejects_unknowns() {
        assert!(RelationKind::from_pg('S').is_err()); // sequence
        assert!(RelationKind::from_pg('i').is_err()); // index
        assert!(RelationKind::from_pg('t').is_err()); // toast
    }
}
```

### 3. `src-tauri/tests/introspect_integration.rs` — new file

Create the file. Pattern matches `pg_integration.rs` and `smoke_select_1.rs` — the `dsn()` / `skip_note()` helpers are duplicated, not factored out (no `tests/common/` infrastructure in this repo).

```rust
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
use quill_lib::pg::PgConnector;
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
        ssl_mode: sqlx::postgres::PgSslMode::Disable,
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_databases_returns_postgres_and_excludes_template0() {
    let Some(dsn) = dsn() else { skip_note(); return };
    let connector = connector_from(&dsn);
    let mut conn = connector.connect(&dsn.database).await.expect("connect");

    let dbs = introspect::list_databases(&mut conn).await.expect("list_databases");
    let names: Vec<&str> = dbs.iter().map(|d: &DatabaseInfo| d.name.as_str()).collect();

    assert!(names.contains(&"postgres"), "postgres should be listed; got {names:?}");
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
    let Some(dsn) = dsn() else { skip_note(); return };
    let connector = connector_from(&dsn);
    let mut conn = connector.connect(&dsn.database).await.expect("connect");

    let payload: SchemaPayload = introspect::introspect_database(&mut conn)
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
            "system schema leaked into introspection: {}", s.name
        );
    }
    let _ = public; // existence is the assertion

    PgConnector::close(conn).await;
}

#[tokio::test]
async fn introspect_database_distinguishes_table_view_matview_function() {
    let Some(dsn) = dsn() else { skip_note(); return };
    let connector = connector_from(&dsn);
    let mut conn = connector.connect(&dsn.database).await.expect("connect");

    // Build a small fixture inside a transient schema so we don't pollute
    // the connecting user's `public`.  ROLLBACK at the end discards it.
    //
    // sqlx's transaction wrapper would make this nicer, but we want the
    // introspection call to see the uncommitted DDL — the same connection
    // sees its own transaction, so this works inside a manual BEGIN.
    sqlx::query("BEGIN").execute(&mut conn).await.expect("begin");
    sqlx::query("CREATE SCHEMA quill_m22_fixture").execute(&mut conn).await.expect("create schema");
    sqlx::query("CREATE TABLE quill_m22_fixture.t1 (id int)").execute(&mut conn).await.expect("create table");
    sqlx::query("CREATE VIEW quill_m22_fixture.v1 AS SELECT 1 AS x").execute(&mut conn).await.expect("create view");
    sqlx::query("CREATE MATERIALIZED VIEW quill_m22_fixture.m1 AS SELECT 1 AS x").execute(&mut conn).await.expect("create matview");
    sqlx::query("CREATE FUNCTION quill_m22_fixture.f1() RETURNS int LANGUAGE sql AS 'SELECT 1'").execute(&mut conn).await.expect("create function");
    sqlx::query("CREATE PROCEDURE quill_m22_fixture.p1() LANGUAGE sql AS ''").execute(&mut conn).await.expect("create procedure");

    let payload = introspect::introspect_database(&mut conn)
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
    assert_eq!(fns_by_name("f1").map(|f| f.kind), Some(FunctionKind::Function));
    assert_eq!(fns_by_name("p1").map(|f| f.kind), Some(FunctionKind::Procedure));

    sqlx::query("ROLLBACK").execute(&mut conn).await.expect("rollback");
    PgConnector::close(conn).await;
}

#[tokio::test]
async fn introspect_database_serializes_to_json_string() {
    // Round-trip through the same path commands::set_schema_cache will take.
    let Some(dsn) = dsn() else { skip_note(); return };
    let connector = connector_from(&dsn);
    let mut conn = connector.connect(&dsn.database).await.expect("connect");

    let payload = introspect::introspect_database(&mut conn)
        .await
        .expect("introspect_database");
    let json = serde_json::to_string(&payload).expect("serialize");
    let back: SchemaPayload = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(payload.v, back.v);
    assert_eq!(payload.schemas, back.schemas);

    PgConnector::close(conn).await;
}
```

## Implementation order

There are **no intermediate compile errors** if you follow this sequence.

1. **`src-tauri/src/introspect/mod.rs`** — write the new file. Not yet compiled (no `mod introspect;` in `lib.rs`); you can write it freely.
2. **`src-tauri/src/lib.rs`** — add `pub mod introspect;`. Verify `( cd src-tauri && cargo build )` succeeds; this is the first point the introspect module is type-checked.
3. **Unit tests inside `introspect/mod.rs`** — already included in step 1. Run `( cd src-tauri && cargo test introspect )` to confirm payload (de)serialization passes.
4. **`src-tauri/tests/introspect_integration.rs`** — write the new file. Run `./test.sh` to confirm unit tests still pass and the integration tests cleanly skip without `QUILL_TEST_PG_URL`. Then start a Docker Postgres and re-run with the env var set.

## Known gotchas

- **`pg_class.relkind` is a `"char"` (single-byte char type), not `text`.** sqlx maps it to `i8` by default, not `String`. The `::text` cast in the SQL gives us a one-character `String` we can `chars().next()` on without depending on the `i8` mapping. Same pattern for `pg_proc.prokind`. If you drop the cast, sqlx returns `Err(ColumnDecode { .. })` at runtime — invisible until you actually run the query.
- **`pg_namespace.nspname NOT LIKE 'pg\_%' ESCAPE '\'`** is the only correct way to exclude `pg_*` schemas with `LIKE`. Without the `ESCAPE`, the underscore is a single-char wildcard and `pg%` would over-match anything starting with `pg`. Worse, future schemas named `pgaudit` or `pgcron` extension internals would all leak through.
- **Excluding `pg_toast_temp_*` and `pg_temp_*`** is implicit — they all start with `pg_`, so the wildcard already filters them.
- **`template1` is connectable but excluded from the visible database list.** Users don't want to see it as a working DB. Exclusion is via `NOT datistemplate`, which catches both `template0` and `template1`. If a user creates their own template (rare), it's also hidden — the same DBeaver behaviour.
- **`relkind = 'p'` is partitioned table, not partition.** Individual partitions are `relkind = 'r'` and *are* listed (each partition is queryable as its own table). The parent partitioned table is also listed with kind `partitioned_table`. The frontend can decide whether to nest partitions under their parent — for M2 they're flat.
- **`relkind = 'f'` (foreign table) is excluded** in v1. If you have an org-mandated foreign-data wrapper, it'll be invisible until v1.1. Document the limitation if a user asks.
- **Overloaded functions appear multiple times.** No deduplication. `MILESTONES.md` accepts this; tracking arg signatures is M4's autocomplete prep.
- **No transaction is opened in the public introspection path.** The three queries are independent reads against `pg_catalog`; consistency between schemas/relations/functions across that ~10ms window doesn't matter for v1. (A new table appearing partway through would surface either in `relations` or not at all — never in `schemas` without its relations.) The integration test that uses a fixture *does* wrap its DDL + introspect in a `BEGIN`/`ROLLBACK` so it sees its own changes; that's a test concern, not a library invariant.
- **The connection passed to introspection must be bound to the target database.** `pg_namespace`, `pg_class`, and `pg_proc` are per-database catalogs; querying them from `postgres` gives you `postgres`'s schemas, not whatever DB the user clicked. M2.3's command layer must acquire a slot bound to the target DB before calling `introspect_database`.
- **`fetch_all` reads everything into memory.** Catalog queries on a huge schema return tens of thousands of rows, but each row is ~50–100 bytes, so worst-case is a few MB — comfortably under the few-hundred-KB-to-few-MB estimate in `MILESTONES.md`. No streaming needed in v1.
- **`PAYLOAD_VERSION = 1`** is intentionally a `pub const`, not a hard-coded literal. M4 will bump it; tests reference the constant so they don't need an update on bump.
- **`SchemaPayload` is `#[derive(Serialize, Deserialize)]`** with no `rename_all` on the top struct, so JSON field names are snake_case (matching `v`, `schemas`, `name`, `relations`, `functions`). The enum variants use `#[serde(rename_all = "snake_case")]` to make `partitioned_table` and `matview` look right in JSON.
- **`introspect_database` uses a `BTreeMap` to dedup schemas and impose a stable order.** Don't switch to `HashMap` — payload determinism matters for the cache (the same DB introspected twice should produce byte-identical JSON, so cache writes don't churn).
- **`PgConnection::close` is consumed by value.** `PgConnector::close(conn).await` matches the trait signature. Tests use this pattern at the end; don't call `conn.close()` directly.
- **No new dependency.** Do not add `chrono`, `time`, `tracing`, or anything else. `serde_json` (already in deps) handles serialization. `thiserror` is already in deps for `IntrospectError`. `sqlx::Row` is needed for `try_get` by column name — already in scope.
- **Tests run from `src-tauri/`.** `cargo test` picks up everything under `src-tauri/tests/` automatically; no test-runner config.
- **`tokio::test` macro** requires features `macros` and `rt` — already present in the dep set.

## Tests

Run via `./test.sh` (and with `QUILL_TEST_PG_URL` set against a real Postgres for the integration tests). Coverage:

**Unit tests (always run):**
- `payload_round_trips_through_json` — serialize + deserialize a full payload.
- `relation_kind_serializes_as_snake_case` — checks `partitioned_table` and `matview` JSON forms.
- `function_kind_serializes_as_snake_case` — checks `window`.
- `relkind_from_pg_rejects_unknowns` — sequences, indexes, toast.

**Integration tests (skipped without `QUILL_TEST_PG_URL`):**
- `list_databases_returns_postgres_and_excludes_template0` — proves the database filter and ordering.
- `introspect_database_returns_public_schema_with_v1_payload` — `public` is present; no system schemas leak through; payload version is `PAYLOAD_VERSION`.
- `introspect_database_distinguishes_table_view_matview_function` — creates a transient schema with one of each kind, asserts they come back with the right `RelationKind` / `FunctionKind`. Wraps the DDL in `BEGIN ... ROLLBACK` so the test is non-destructive.
- `introspect_database_serializes_to_json_string` — same round-trip the M2.3 cache write will do.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds on a machine **without** `QUILL_TEST_PG_URL` — the four integration tests log "QUILL_TEST_PG_URL not set; skipping" and pass.
- [ ] `QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh` succeeds against a local `postgres:17` Docker container — all four integration tests report `ok`, and the existing `pg_integration` + `smoke_select_1` tests still pass.
- [ ] `grep -RIn "information_schema" src-tauri/src/introspect` shows only the *exclusion* patterns (negative `<>` checks), not any query that reads from `information_schema`.
- [ ] `grep -RIn "psql\|\\\\d" src-tauri/src/introspect` returns no matches (no shell-outs).
- [ ] `grep -n "PAYLOAD_VERSION" src-tauri/src/introspect/mod.rs` shows the constant declaration and at least one use inside `introspect_database`.
- [ ] `grep -c "pub async fn" src-tauri/src/introspect/mod.rs` returns `2` — exactly `list_databases` and `introspect_database` are public.
- [ ] No Tauri commands added (M2.3 owns the command surface).
- [ ] No new dependencies in `Cargo.toml`.
- [ ] `src-tauri/src/lib.rs` declares `pub mod introspect;` and nothing else changes there.
- [ ] Exactly two new files exist: `src-tauri/src/introspect/mod.rs`, `src-tauri/tests/introspect_integration.rs`.

## Out of scope

- Column metadata for relations — **M4**. The cache payload version stays at 1 in M2; M4 bumps to v2 and adds `columns: Vec<ColumnInfo>` per relation.
- `search_path` capture — **M4**. M2 returns raw catalog contents in whatever order Postgres holds them; the tree doesn't honour `search_path` until autocomplete cares.
- Function argument signatures, return types, language — **M4** at the earliest. Overloads appear multiple times.
- Indexes, sequences, types, extensions — explicitly not v1 (`PRD.md` §3 non-goals: "ER diagrams" and "deep object editors" are the broader exclusions).
- Tauri commands wrapping these functions — **M2.3**.
- Any cache I/O — **M2.3**. `introspect::*` knows nothing about `store::SchemaCacheRow` or `store::set_schema_cache`.
- Frontend code — **M2.4**.
