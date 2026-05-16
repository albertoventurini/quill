# M1.4 — Postgres binding for slot manager

## Goal
Provide the real `Connector` implementation (using `sqlx::PgConnection`) so the slot manager from M1.3 can actually open Postgres connections.

## Context to read first
- `PRD.md` §6 (slot model) and §9 (tech stack).
- `AGENTS.md` — principles 1 (no hidden connections) and 2 (pool is a budget).
- `tasks/m1-3-slot-manager.md` — defines the `Connector` trait this task implements.

## Critical constraint
**One `PgConnection` per slot — NOT a `PgPool`.** The slot manager *is* the pool; using sqlx's own pool here would defeat the budget. Build connect options manually each time.

## Deliverables

### 1. Module
`src-tauri/src/pg/mod.rs`:
```rust
pub struct PgConnector {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: SecretString,
    pub ssl_mode: sqlx::postgres::PgSslMode,
}

#[async_trait::async_trait]
impl Connector for PgConnector {
    type Conn = sqlx::PgConnection;
    async fn connect(&self, database: &str) -> Result<Self::Conn, ConnectorError> { ... }
    async fn close(conn: Self::Conn) { let _ = conn.close().await; }
}
```

Build options via `sqlx::postgres::PgConnectOptions::new()
    .host(...).port(...).database(database)
    .username(...).password(self.password.expose_secret())
    .ssl_mode(self.ssl_mode)
    .application_name("quill")`
and connect with `PgConnection::connect_with(&opts).await`.

Also call `.disable_statement_logging()` on the options to keep dev output clean.

### 2. Cancel handle (best-effort for M1)
Postgres exposes an out-of-band `CancelRequest` mechanism. sqlx 0.8 surfaces this via `PgConnection`'s underlying stream; if the API isn't ergonomic, add a TODO and a `cancel_token()` stub that returns `Option<CancelToken>`. The real cancel UX lands in M3; M1 just needs to not paint into a corner.

### 3. Secret handling
- Use `secrecy::SecretString` so the password never lands in `Debug` output.
- For M1, the password reaches `PgConnector` in-process from the user's input (via M1.5's `connect_server` command). M6 will swap to the OS keychain.

### 4. ServerRegistry
`src-tauri/src/registry.rs`:
```rust
pub struct ServerHandle { pub slot_manager: SlotManager<PgConnector> }

#[derive(Default)]
pub struct ServerRegistry { pub by_id: dashmap::DashMap<i64, ServerHandle> }
```
Register via `app.manage(ServerRegistry::default())` during Tauri `setup`. The registry stays empty until M1.5's `connect_server` populates it.

## Tests

### Integration (gated)
`src-tauri/tests/pg_integration.rs`. Gate every test on `std::env::var("QUILL_TEST_PG_URL")` — `#[ignore]` is fine if the env var is absent. A local Postgres for tests is available via Docker:
```bash
docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17
```
Default DSN: `postgres://postgres:dev@localhost:5432/postgres`.

Coverage:
- `PgConnector::connect("postgres")` returns a usable connection; `SELECT 1` works.
- `SlotManager<PgConnector>` with budget=2: open two distinct DBs, both succeed.
- With budget=2 and both slots idle, opening a third DB evicts the LRU (close on the real socket).

Run with:
```bash
QUILL_TEST_PG_URL="postgres://postgres:dev@localhost:5432/postgres" ./test.sh
```

## Dependencies (extend `Cargo.toml`)
- Extend sqlx features: `["postgres", "runtime-tokio", "macros", "migrate", "sqlite"]`.
- `secrecy = "0.10"` (or latest stable).
- `dashmap = "6"`.

## Acceptance criteria
- [ ] `./test.sh` passes (unit tests still green).
- [ ] Integration tests pass against local Docker Postgres when `QUILL_TEST_PG_URL` is set.
- [ ] `PgConnector` implements `Connector` from M1.3 verbatim.
- [ ] No `PgPool` anywhere — only `PgConnection`s via the slot manager.
- [ ] Passwords never appear in `Debug` output (use `SecretString`).
- [ ] `ServerRegistry` registered as Tauri managed state.

## Out of scope
- Tauri commands using the registry — M1.5.
- OS keychain for passwords — M6.
- Cancellation UX — M3.
