# M3.1 — Migrate Postgres I/O from sqlx-postgres to tokio-postgres

## Goal

**Before:** All Postgres work goes through `sqlx`. `PgConnector::Conn = sqlx::PgConnection` (`pg/mod.rs`), introspection queries are `sqlx::query(...)`-driven (`introspect/mod.rs`), and `run_query` calls `sqlx::query(&sql).fetch_all(&mut *guard)` over `PgRow` (`commands/mod.rs`). `Cargo.toml` enables the `postgres` feature on `sqlx 0.8`.

**After:** All Postgres work goes through `tokio-postgres 0.7`. `PgConnector::Conn = tokio_postgres::Client` and the rest of the call sites are updated to that API. `sqlx` keeps only its `sqlite` + `runtime-tokio` features — it's still the local-store driver, but no longer touches Postgres. **No new user-visible behavior** — same SQL, same JSON output, same SSL handling. `./test.sh` still passes; the smoke test from M2.4 still works end-to-end.

This task is a **pure refactor**. It does *not* yet capture cancel credentials (M3.2), expose `cancel_query` (M3.3), stream/paginate (M3.4), or change any UI (M3.5/M3.6). The only reason it lands first is that every later M3 task depends on having `Client::cancel_token()` available.

## Current state

### `src-tauri/Cargo.toml`

```toml
sqlx = { version = "0.8", features = ["postgres", "sqlite", "runtime-tokio", "macros", "migrate"] }
```

### `src-tauri/src/pg/mod.rs` (in full — gets rewritten)

Uses `sqlx::PgConnection::connect_with(PgConnectOptions::...)` and stores the connection directly. SSL mode is parsed into `sqlx::postgres::PgSslMode`. `close` calls `conn.close().await`. Ends with `// TODO(M3): Cancellation plumbing…`.

### `src-tauri/src/introspect/mod.rs`

Issues four catalog queries against `&mut sqlx::PgConnection`. The functions take `conn: &mut PgConnection` and use `sqlx::query_as` / `sqlx::query`. SQL strings are unchanged from M2 — only the execution path migrates.

### `src-tauri/src/commands/mod.rs`

- `pg_row_to_json` switches on `col.type_info().name()` strings (`"BOOL"`, `"INT4"`, …) and uses `row.try_get::<T, _>(i)`.
- `run_query` calls `sqlx::query(&sql).fetch_all(&mut *guard)` — `*guard` is `sqlx::PgConnection` today.
- `run_introspection` calls `introspect::introspect_database(&mut guard)`.

### `src-tauri/src/slots/mod.rs`

`Connector::Conn` is a generic associated type. **Nothing in slots changes for this task** — the slot manager is connector-agnostic. The only edit is in tests, if any, that name `PgConnection` (there are none — slots are tested with `FakeConnector`).

## Design choices baked into this spec

- **`tokio-postgres` not `deadpool-postgres`, not `bb8`.** A pool defeats the budget (AGENTS.md principle 2). The slot manager *is* the pool.
- **TLS via `tokio-postgres-rustls`.** Matches sqlx's default rustls backend; pure-Rust, no OpenSSL. `webpki-roots` supplies system roots without OS plumbing. `disable` / `prefer` use `NoTls`; `require` / `verify-ca` / `verify-full` use rustls. v1 limitation: `verify-ca` and `verify-full` use webpki roots only — no custom root certificates. Document it; M6 polish can add custom roots.
- **`Connection` half spawned on a tokio task.** `tokio_postgres::connect` returns `(Client, Connection)`; the `Connection` future drives the socket and must be polled. Spawn it. If it errors, log to stderr — that's all v1 does. The `Client` is what the slot stores.
- **`close` is no-op.** Dropping `Client` closes the socket; the `Connection` task notices and exits. No explicit `client.close()` exists. Adjust `Connector::close` to just consume the value.
- **Type mapping requires opt-in features.** `tokio-postgres` only decodes builtin Rust types out of the box (`bool`, integers, `&str` for text, `Vec<u8>` for bytea, `serde_json::Value` for json with `with-serde_json-1`). For UUID / date / time / numeric we add:
  - `tokio-postgres` features `with-uuid-1`, `with-chrono-0_4`, `with-serde_json-1`
  - `rust_decimal` with `db-tokio-postgres` for `NUMERIC`
  - `chrono` for date/time `to_string()`
  - `uuid` for UUID `to_string()`
  Map binary types to their Display-formatted string — exactly what sqlx was doing via its `String` decoder.
- **OID is decoded as `u32`, formatted as decimal string.** sqlx routed it through `String`; tokio-postgres has no `&str` decode for OID.
- **Connector errors carry the underlying tokio-postgres error message verbatim.** The frontend already renders these via `CommandError.message`.

## Deliverables

### 1. `src-tauri/Cargo.toml` — dependency updates

Replace the sqlx line and add the tokio-postgres stack:

```toml
[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-opener = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio", "macros", "migrate"] }

tokio-postgres = { version = "0.7", features = [
    "with-uuid-1",
    "with-chrono-0_4",
    "with-serde_json-1",
] }
tokio-postgres-rustls = "0.12"
rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12"] }
webpki-roots = "0.26"

async-trait = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }
thiserror = "2"
secrecy = "0.10"
dashmap = "6"
base64 = "0.22"
chrono = { version = "0.4", default-features = false, features = ["std", "clock"] }
uuid = { version = "1" }
rust_decimal = { version = "1", default-features = false, features = ["db-tokio-postgres", "std"] }
```

Notes:
- The **`postgres` feature is removed from sqlx**. The local store (SQLite) is untouched; tests for `store/mod.rs` still pass.
- `rustls` default-features-off keeps build time reasonable; `ring` is the only crypto backend we need.
- `chrono` is pulled in transitively by `tokio-postgres` with `with-chrono-0_4`; declared explicitly so we can `use chrono::*` in `commands/mod.rs`.

### 2. `src-tauri/src/pg/mod.rs` — full rewrite

```rust
//! Real Postgres `Connector` implementation for the slot manager.
//!
//! M3.1 migration: switched from `sqlx::PgConnection` to
//! `tokio_postgres::Client`.  The driving reason is M3.2/M3.3 cancellation —
//! tokio-postgres exposes `Client::cancel_token()` (backend PID + secret key
//! captured during the protocol startup handshake), which sqlx 0.8 hides
//! behind crate-private fields.  See `MILESTONES.md` §M3.
//!
//! AGENTS.md principle 2: the slot manager *is* the pool.  This module
//! deliberately uses a raw `Client`, never `tokio_postgres::Pool` — there is
//! no built-in pool in tokio-postgres anyway.

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use tokio_postgres::{Client, Config, NoTls, config::SslMode};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::slots::{Connector, ConnectorError};

/// Logical SSL policy parsed from the textual `ssl_mode` stored on
/// `connections.ssl_mode`.  Mirrors libpq.
#[derive(Debug, Clone, Copy)]
pub enum SslPolicy {
    Disable,
    Allow,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl SslPolicy {
    /// Map to tokio-postgres' [`SslMode`].  `VerifyCa` / `VerifyFull` both
    /// degrade to `Require` because v1 does not ship custom root certs;
    /// the spec writer should re-examine when M6 polish lands.
    fn as_tokio(self) -> SslMode {
        match self {
            SslPolicy::Disable => SslMode::Disable,
            SslPolicy::Allow | SslPolicy::Prefer => SslMode::Prefer,
            SslPolicy::Require | SslPolicy::VerifyCa | SslPolicy::VerifyFull => SslMode::Require,
        }
    }

    /// Whether we need to build a TLS connector at all for this policy.
    /// `Disable` skips TLS entirely; `Prefer` *may* upgrade if the server
    /// supports it, so we still pass a connector when the user asked for it.
    fn wants_tls(self) -> bool {
        !matches!(self, SslPolicy::Disable)
    }
}

pub struct PgConnector {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: SecretString,
    pub ssl_mode: SslPolicy,
}

impl PgConnector {
    /// Map the textual `ssl_mode` stored in the SQLite `connections` table
    /// to the typed [`SslPolicy`].  Accepts the same spellings as libpq.
    pub fn parse_ssl_mode(s: &str) -> Result<SslPolicy, ConnectorError> {
        Ok(match s {
            "disable" => SslPolicy::Disable,
            "allow" => SslPolicy::Allow,
            "prefer" => SslPolicy::Prefer,
            "require" => SslPolicy::Require,
            "verify-ca" => SslPolicy::VerifyCa,
            "verify-full" => SslPolicy::VerifyFull,
            other => return Err(ConnectorError(format!("unknown ssl_mode: {other}"))),
        })
    }

    fn build_config(&self, database: &str) -> Config {
        let mut config = Config::new();
        config
            .host(&self.host)
            .port(self.port)
            .dbname(database)
            .user(&self.username)
            .password(self.password.expose_secret())
            .application_name("quill")
            .ssl_mode(self.ssl_mode.as_tokio());
        config
    }
}

#[async_trait]
impl Connector for PgConnector {
    type Conn = Client;

    async fn connect(&self, database: &str) -> Result<Self::Conn, ConnectorError> {
        let config = self.build_config(database);

        if self.ssl_mode.wants_tls() {
            let tls = make_rustls()
                .map_err(|e| ConnectorError(format!("rustls setup failed: {e}")))?;
            let (client, connection) = config
                .connect(tls)
                .await
                .map_err(|e| ConnectorError(e.to_string()))?;
            spawn_connection_driver(connection);
            Ok(client)
        } else {
            let (client, connection) = config
                .connect(NoTls)
                .await
                .map_err(|e| ConnectorError(e.to_string()))?;
            spawn_connection_driver(connection);
            Ok(client)
        }
    }

    /// tokio-postgres has no explicit `close` — dropping the [`Client`]
    /// causes the spawned connection task to exit on its next poll.  We just
    /// consume the value so callers can stop holding it.
    async fn close(_conn: Self::Conn) {
        // intentionally empty
    }
}

/// Spawn the driver future returned alongside the `Client`.  The driver is
/// what actually pumps bytes between the socket and the client; without it
/// queries hang forever.  Errors are logged to stderr — there is no UI path
/// for "connection silently went away" in v1.
fn spawn_connection_driver<S, T>(connection: tokio_postgres::Connection<S, T>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    T: tokio_postgres::tls::TlsStream + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("[quill] postgres connection task ended with error: {e}");
        }
    });
}

/// Build a rustls-based TLS connector.  Uses webpki-roots — no custom CA
/// support in v1.
fn make_rustls() -> Result<MakeRustlsConnect, Box<dyn std::error::Error>> {
    use rustls::ClientConfig;

    // Install the default crypto provider once per process.  Calling this
    // twice is harmless (it returns Err that we ignore); calling it never
    // is fatal at handshake time.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(MakeRustlsConnect::new(config))
}
```

### 3. `src-tauri/src/introspect/mod.rs` — convert four queries

The SQL strings stay byte-identical to M2.2. Only the execution path changes.

- The function parameter changes from `conn: &mut sqlx::PgConnection` to `client: &tokio_postgres::Client` (note: `&`, not `&mut` — `tokio_postgres::Client` shares borrowed access for queries).
- Replace `sqlx::query(SQL).fetch_all(conn).await?` with `client.query(SQL, &[]).await?`.
- Replace `row.get::<String, _>("col")` with `row.try_get::<&str, &str>("col")?.to_string()` (column name lookup is supported in tokio-postgres just like sqlx).
- For `relkind` and `prokind` (which are `"char"` in Postgres — single-byte), pull them via `row.try_get::<&str, i8>("relkind")? as u8 as char`. tokio-postgres maps Postgres `"char"` to `i8`.
- The `IntrospectError` enum: change `#[from] sqlx::Error` to `#[from] tokio_postgres::Error`. Existing variants and call sites unaffected — the enum stays `serde::Serialize` via the same `to_string()` path.

Skeleton for one function (the others follow the same pattern):

```rust
pub async fn list_databases(
    client: &tokio_postgres::Client,
) -> Result<Vec<DatabaseInfo>, IntrospectError> {
    let rows = client
        .query(
            "SELECT datname FROM pg_database \
             WHERE datallowconn AND NOT datistemplate \
             ORDER BY datname",
            &[],
        )
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| DatabaseInfo {
            name: r.get::<_, &str>("datname").to_string(),
        })
        .collect())
}
```

The `introspect_database` function (which builds the full `SchemaPayload`) is the largest change — it joins relations, functions, and schemas. Drop in tokio-postgres' `client.query(...)`-based execution for each catalog query the same way. **Don't change the SQL.**

### 4. `src-tauri/src/commands/mod.rs` — row mapping + `run_query`

Imports:

```rust
use tokio_postgres::Row;
use tokio_postgres::types::Type;
```

Replace the `pg_row_to_json` body. Same external signature (`pub fn pg_row_to_json(row: &Row) -> Vec<Value>`), same behavior, new internals:

```rust
pub fn pg_row_to_json(row: &Row) -> Vec<Value> {
    let columns = row.columns();
    let mut values = Vec::with_capacity(columns.len());

    for (i, col) in columns.iter().enumerate() {
        let val = match *col.type_() {
            Type::BOOL => option_to_json(row.try_get::<_, Option<bool>>(i), Value::Bool),
            Type::INT2 => option_to_json(row.try_get::<_, Option<i16>>(i), |v| {
                Value::Number((v as i64).into())
            }),
            Type::INT4 => option_to_json(row.try_get::<_, Option<i32>>(i), |v| {
                Value::Number((v as i64).into())
            }),
            Type::INT8 => option_to_json(row.try_get::<_, Option<i64>>(i), |v| {
                Value::Number(v.into())
            }),
            Type::OID => option_to_json(row.try_get::<_, Option<u32>>(i), |v| {
                Value::Number((v as i64).into())
            }),
            Type::FLOAT4 => option_to_json(row.try_get::<_, Option<f32>>(i), |v| {
                serde_json::Number::from_f64(v as f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }),
            Type::FLOAT8 => option_to_json(row.try_get::<_, Option<f64>>(i), |v| {
                serde_json::Number::from_f64(v)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }),

            Type::JSON | Type::JSONB => {
                option_to_json(row.try_get::<_, Option<Value>>(i), |v| v)
            }

            Type::BYTEA => option_to_json(row.try_get::<_, Option<Vec<u8>>>(i), |bytes| {
                use base64::Engine;
                Value::String(base64::engine::general_purpose::STANDARD.encode(&bytes))
            }),

            Type::UUID => option_to_json(row.try_get::<_, Option<uuid::Uuid>>(i), |u| {
                Value::String(u.to_string())
            }),

            Type::DATE => option_to_json(
                row.try_get::<_, Option<chrono::NaiveDate>>(i),
                |d| Value::String(d.to_string()),
            ),
            Type::TIME => option_to_json(
                row.try_get::<_, Option<chrono::NaiveTime>>(i),
                |t| Value::String(t.to_string()),
            ),
            Type::TIMESTAMP => option_to_json(
                row.try_get::<_, Option<chrono::NaiveDateTime>>(i),
                |t| Value::String(t.to_string()),
            ),
            Type::TIMESTAMPTZ => option_to_json(
                row.try_get::<_, Option<chrono::DateTime<chrono::Utc>>>(i),
                |t| Value::String(t.to_rfc3339()),
            ),

            Type::NUMERIC => option_to_json(
                row.try_get::<_, Option<rust_decimal::Decimal>>(i),
                |d| Value::String(d.to_string()),
            ),

            // Text-family types
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::CHAR => {
                option_to_json(row.try_get::<_, Option<&str>>(i), |s| {
                    Value::String(s.to_string())
                })
            }

            // Unknown — best-effort &str fallback.
            _ => match row.try_get::<_, Option<&str>>(i) {
                Ok(Some(s)) => Value::String(s.to_string()),
                Ok(None) => Value::Null,
                Err(_) => Value::Null,
            },
        };
        values.push(val);
    }

    values
}

/// Tiny helper: collapse `Result<Option<T>, _>` into `serde_json::Value`
/// via a converter for the `Some` branch.  `Err` and `None` both become
/// `Value::Null` — the user always sees something.
fn option_to_json<T, F: FnOnce(T) -> Value>(
    r: Result<Option<T>, tokio_postgres::Error>,
    f: F,
) -> Value {
    match r {
        Ok(Some(v)) => f(v),
        Ok(None) | Err(_) => Value::Null,
    }
}
```

Replace the query execution inside `run_query`. The slot guard now derefs to `Client`, not `PgConnection`. Since `Client::query` takes `&self` (not `&mut`), call it directly on `&*guard`:

```rust
let start = Instant::now();
let rows: Vec<Row> = guard
    .query(&sql, &[])
    .await
    .map_err(|e| CommandError::Pg(e.to_string()))?;
let duration_ms = start.elapsed().as_millis() as u64;

let columns: Vec<ColumnMeta> = rows
    .first()
    .map(|r| {
        r.columns()
            .iter()
            .map(|col| ColumnMeta {
                name: col.name().to_string(),
                type_name: col.type_().name().to_string(),
            })
            .collect()
    })
    .unwrap_or_default();

let row_count = rows.len();
let json_rows: Vec<Vec<Value>> = rows.iter().map(pg_row_to_json).collect();
```

`run_introspection` likewise drops the `&mut guard` to `&*guard`:

```rust
Ok(introspect::introspect_database(&*guard).await?)
```

The `bare SELECT` rejection block stays as-is; the `Connector::connect` chain still returns `ConnectorError` so the slot error path is unchanged.

`list_databases` (the Tauri command) updates the introspect call site the same way — drop `&mut guard` to `&*guard`.

### 5. `Connector::Conn` is unchanged in `slots/mod.rs`

The trait stays generic — `Connector::Conn = Client` is set in `pg/mod.rs`. The `SlotGuard<C: Connector>` exposes `Deref<Target = C::Conn> = Client`, so call sites read `&*guard` to get `&Client`.

**Important**: review the `SlotGuard::deref_mut` usage. With `sqlx::PgConnection` we needed `&mut` to run a query; with `tokio_postgres::Client` we only need `&`. The trait still exposes both; existing callers that say `&mut *guard` keep compiling because `&mut Client` auto-coerces in any context that wants `&Client`. **For new call sites, prefer `&*guard`.**

### 6. Type-name backwards compatibility

`ColumnMeta.type_name` is sent to the frontend and rendered as-is in cell tooltips / future grid headers. tokio-postgres' `Type::name()` returns lowercase names (`"int4"`, `"text"`) versus sqlx's uppercase (`"INT4"`, `"TEXT"`). To avoid breaking the frontend (which currently does not branch on these — but might in M3.5), **emit the uppercase form**:

```rust
type_name: col.type_().name().to_uppercase(),
```

Document the call in a comment. Future M3.5 grid code can rely on `INT4` / `TEXT` and friends as it does today.

## Implementation order

1. **Cargo.toml** — update dependencies. Run `( cd src-tauri && cargo fetch )` — must resolve cleanly. **Do not** build yet; the source tree won't compile until the rewrites are in.
2. **`pg/mod.rs`** — full rewrite. `cargo check` after — fails on call sites in `commands` and `introspect`, which is expected.
3. **`introspect/mod.rs`** — convert the four catalog query functions and the `introspect_database` orchestrator. `cargo check` — now only `commands/mod.rs` should fail.
4. **`commands/mod.rs`** — replace `pg_row_to_json`, update `run_query` body, update `run_introspection`, update `list_databases`. `cargo build` — must succeed clean.
5. **`cargo test`** — the slot manager tests still use `FakeConnector` and pass unchanged. Store tests are untouched. Introspect integration tests (M2.2) **need updating** — they pass `&mut PgConnection` today; flip them to `&Client`.
6. **`./test.sh`** — full pass.
7. **Smoke test** — `./run.sh` + the M2.4 manual procedure end-to-end. Connect, expand tree, run a query against a fixture DB. Run a query with mixed types: `SELECT 1::int4, 'hello'::text, '2026-05-19'::date, 'a1b2c3'::uuid, '3.14'::numeric, true::bool, '\xdeadbeef'::bytea` — every column must render correctly in the result `<pre>`.

## Known gotchas

- **`Client::query` takes `&self`, not `&mut self`.** sqlx's `PgConnection::query` was `&mut`. Existing call sites using `&mut *guard` keep compiling but read awkwardly; prefer `&*guard`.
- **The Connection task lives independently of the Client.** Dropping the `Client` closes the socket; the spawned task notices on its next poll and exits. There is no leak. If `tokio::spawn`'s receiver were ever wired to a oneshot for error reporting, that channel would have to be selected on inside the slot — not in v1.
- **rustls default crypto provider must be installed.** Call `rustls::crypto::ring::default_provider().install_default()` once per process — `make_rustls` does this. Calling it twice returns `Err(())`; ignore.
- **`webpki-roots` returns a constant array.** It is extended into the root store with `root_store.extend(...)`. The roots are the Mozilla CA list — same set rustls uses elsewhere in our stack.
- **NUMERIC requires `rust_decimal` with the `db-tokio-postgres` feature.** Without the feature flag, the `FromSql` impl isn't compiled in and `row.try_get::<_, Decimal>(i)` won't find one. Verify by `( cd src-tauri && cargo tree -e features | grep rust_decimal )` showing the feature enabled.
- **UUID / chrono types come from tokio-postgres' opt-in features.** The `with-uuid-1` and `with-chrono-0_4` features compile their respective `FromSql` impls into `tokio_postgres::types`. Importing `uuid::Uuid` and `chrono::*` in `commands/mod.rs` is what wires them through.
- **OID is `u32` in tokio-postgres.** sqlx pulled it as `String`. The new branch maps `Type::OID` via `u32` and converts to `Value::Number((v as i64).into())`. This is a behavior tweak — tooltips that read OIDs as text get integers now. If you want to match the old shape exactly, route through `format!("{v}")` instead. Either is fine; pick one and note in the commit message.
- **`tokio_postgres::Error` is `Send + Sync`.** It works directly with `#[from]` in `thiserror` enums — no `Box`.
- **`Type` is a non-exhaustive enum in tokio-postgres.** The wildcard `_` arm at the bottom of `pg_row_to_json`'s match is required. Don't be tempted to enumerate everything.
- **Postgres' `"char"` type (single byte) and `CHAR` (blank-padded) are different.** `Type::CHAR` in tokio-postgres is the single-byte type; `Type::BPCHAR` is blank-padded `CHAR(n)`. Group them in the text branch — sqlx already collapsed them.
- **`r.get::<_, &str>("col")` *panics* on null in tokio-postgres.** Use `try_get::<_, Option<&str>>(...)` everywhere a column might be null. The catalog queries in `introspect` use `WHERE` filters that exclude nulls, so the bare `.get` form in introspect is safe — but document it with a doc-comment.
- **Don't keep the `sqlx::postgres` import path anywhere.** `grep -RIn 'sqlx::postgres' src-tauri/src` should return zero matches after the migration. The only `sqlx` imports left should be `sqlx::SqlitePool`, `sqlx::migrate`, and the test helpers around them.
- **Existing M2.2 integration tests live behind `#[ignore]` and require a `QUILL_TEST_PG_URL`.** They pass `&mut PgConnection` — flip the helper to `tokio_postgres::connect` + spawn. The URL format is identical (`postgres://user:pass@host:port/db`).
- **No new public surface.** The `Connector` trait, `SlotManager`, `ServerHandle`, every `#[tauri::command]` keep the exact same signatures externally. The Tauri bridge type definitions in `src/lib/tauri.ts` need **zero changes** in this task.
- **Cancellation hook lives on `Client`.** `client.cancel_token()` already returns a `CancelToken` — this is the hook M3.2 will grab. Don't store it yet; M3.2 owns that change.

## Tests

- **`./test.sh` runs as today.** Slot manager unit tests, store tests, command's `is_bare_select` test all pass without modification.
- **Introspect integration tests (M2.2 `tests/pg_introspect.rs`)** need a small connect-helper update from sqlx to tokio-postgres. Same `QUILL_TEST_PG_URL` env var; same `#[ignore]` gating. Walk every test that connects: replace `let mut conn = PgConnection::connect(&url).await?;` with:

  ```rust
  let (client, connection) = tokio_postgres::connect(&url, NoTls).await?;
  tokio::spawn(async move {
      let _ = connection.await;
  });
  ```

  Then call `list_databases(&client)` / `introspect_database(&client)` directly.

- **New unit test in `pg/mod.rs`** (optional but cheap): `SslPolicy::parse_ssl_mode` round-trips all known values and returns `ConnectorError` on garbage. The function is small but is the only purely-pure-Rust thing in this module.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds — no test regressions; `pnpm check` passes unchanged.
- [ ] `grep -RIn 'sqlx::postgres' src-tauri/src` returns **zero** matches.
- [ ] `grep -RIn 'PgConnection' src-tauri/src` returns **zero** matches.
- [ ] `grep -RIn 'sqlx::PgPool' src-tauri/src` returns **zero** matches.
- [ ] `grep -F '"postgres"' src-tauri/Cargo.toml` returns zero matches (the sqlx feature is gone).
- [ ] `( cd src-tauri && cargo tree -i tokio-postgres )` shows the dep is wired and `with-uuid-1`/`with-chrono-0_4`/`with-serde_json-1` features are enabled.
- [ ] Smoke test: `./run.sh`, connect to a local Postgres, run `SELECT 1, 'x', '2026-01-01'::date, gen_random_uuid(), 3.14::numeric, true, '\xff'::bytea` — every cell renders non-empty in the `<pre>` block.
- [ ] M2.4 smoke procedure passes verbatim — no behavior change to the tree or its expansions.
- [ ] No changes to any `src/` file (frontend). `git diff src/` is empty.
- [ ] No changes to `src-tauri/migrations/`.
- [ ] No new `#[tauri::command]` functions; `commands::*` count is the same 12 as M2.4.

## Out of scope

- Capturing the `CancelToken` and stashing it on the slot — **M3.2**.
- The `cancel_query` Tauri command — **M3.3**.
- Cursor-based streaming, `fetch_more`, `close_result` — **M3.4**.
- Frontend changes (CodeMirror, Cancel button, errors) — **M3.5**, **M3.6**.
- Custom-CA support for `verify-ca` / `verify-full` — **M6** polish.
- Switching the local SQLite driver away from sqlx — not happening; sqlx-sqlite stays.
- Replacing `dashmap`, `secrecy`, `thiserror`, or any other unrelated dep — no.
- Migrating the M2.2 `tests/pg_introspect.rs` test gating away from `#[ignore]` — keep the gate; CI doesn't run a real Postgres.
- Pretty-printing dates/timestamps differently from `to_string()` — v1 ships RFC3339 for `TIMESTAMPTZ` and ISO-ish `Display` for the rest; presentational tweaks belong to the M3.5 grid task.
- Multi-statement script execution (PRD §12 open question) — **M6**.
