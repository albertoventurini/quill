# M1.2 — Local SQLite store with `connections` table

## Goal
Add an app-local SQLite database for Quill's own state (saved connections, schema cache, history, saved queries). For M1, only the `connections` table is needed; the others land in later milestones. This task is **headless** — no UI, no Tauri commands. The output is a Rust module plus a migration that runs on startup.

## Context to read first
- `PRD.md` §10 — full local-data schema.
- `AGENTS.md` — design principles and Rust style.

## Deliverables

### 1. Location
SQLite file at `<app_data_dir>/quill.sqlite`, where `<app_data_dir>` comes from Tauri's `app.path().app_data_dir()`. On Linux this is `~/.local/share/com.alberto.quill/`. Create the directory if missing.

### 2. Migration
- Embed migrations as `.sql` under `src-tauri/migrations/`.
- For M1, exactly one file: `0001_initial.sql`:
  ```sql
  CREATE TABLE connections (
      id            INTEGER PRIMARY KEY AUTOINCREMENT,
      name          TEXT    NOT NULL UNIQUE,
      host          TEXT    NOT NULL,
      port          INTEGER NOT NULL DEFAULT 5432,
      default_db    TEXT    NOT NULL,
      username      TEXT    NOT NULL,         -- "user" is reserved in SQL
      ssl_mode      TEXT    NOT NULL DEFAULT 'prefer',
      slot_budget   INTEGER NOT NULL DEFAULT 2,
      password_ref  TEXT,                     -- opaque keyring id; NULL in M1
      created_at    TEXT    NOT NULL DEFAULT (datetime('now'))
  );
  ```
- Run migrations via `sqlx::migrate!()` against this folder during startup.

### 3. Module structure
Create `src-tauri/src/store/mod.rs` exposing:
```rust
pub async fn open(app: &tauri::AppHandle) -> Result<sqlx::SqlitePool, StoreError>;

pub struct Connection { id, name, host, port, default_db, username,
                        ssl_mode, slot_budget, password_ref, created_at }
                        // derive Serialize, Deserialize, Clone, Debug

pub struct NewConnection { /* same minus id, created_at */ }

pub async fn list  (pool: &SqlitePool)                 -> Result<Vec<Connection>, StoreError>;
pub async fn get   (pool: &SqlitePool, id: i64)        -> Result<Option<Connection>, StoreError>;
pub async fn insert(pool: &SqlitePool, c: NewConnection) -> Result<Connection, StoreError>;
pub async fn delete(pool: &SqlitePool, id: i64)        -> Result<(), StoreError>;
```
Pool size: 1 (no contention against the local file). Use `SqlitePoolOptions::new().max_connections(1)`.

### 4. Errors
`StoreError` enum via `thiserror`, wrapping `sqlx::Error` and `std::io::Error`. All public functions return `Result<_, StoreError>`.

### 5. Wire into Tauri startup
In `src-tauri/src/lib.rs`, during the `setup` closure: `open(&app.handle()).await?`, then `app.manage(pool)`. Do **not** expose any Tauri commands yet — that's M1.5.

## Dependencies (add to `src-tauri/Cargo.toml`)
- `sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio", "macros", "migrate"] }`
- `tokio = { version = "1", features = ["rt-multi-thread", "macros"] }`
- `thiserror = "2"`

## Tests
Unit tests in `#[cfg(test)] mod tests` inside `store/mod.rs`. Each test uses `SqlitePool::connect("sqlite::memory:")` and re-runs the migrations against the in-memory DB — no touching the real app-data dir. Cover:
- Migration runs cleanly.
- `insert` returns a row with the new id; `list` shows it; `get(id)` returns it.
- `get(unknown_id)` returns `None`.
- `delete(id)` removes the row; subsequent `get` is `None`.
- UNIQUE on `name` is enforced (second insert with same name → `StoreError` carrying a unique-violation).

Run via `./test.sh`.

## Acceptance criteria
- [ ] `./test.sh` passes.
- [ ] `./run.sh` boots the app and creates `~/.local/share/com.alberto.quill/quill.sqlite` with the `connections` table.
- [x] No Tauri commands exposed yet.
- [x] Style follows AGENTS.md (rustfmt, clippy clean, comments only where the *why* is non-obvious).

## Out of scope
- Other tables (`schema_cache`, `query_history`, `saved_queries`) — later milestones.
- OS keychain for passwords — M6. `password_ref` stays nullable.
- Anything frontend-facing — M1.5/M1.6.

## Design constraints (don't violate)
- Don't add columns not in PRD §10. If a later task needs more, that task can add a migration.
- The local DB never holds the user's Postgres data — only Quill's own metadata.
