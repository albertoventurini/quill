# OpenBao Credential Integration

## Goal

Give Quill a second credential source: **OpenBao** (Vault-compatible dynamic secrets). Each saved connection chooses one of two sources: `"password"` (today's prompt-on-connect flow) or `"openbao"` (fetch credentials from an OpenBao server before connecting). The OpenBao token is obtained via browser-based SSO — Quill opens the org's IdP in the default browser, runs a local `127.0.0.1` HTTP callback server, and captures the resulting Vault token automatically. Credentials carry a TTL; the UI shows a countdown and offers a manual "Refresh" action.

---

## Design constraints (from PRD.md + AGENTS.md)

- **No hidden connections.** Every `fetch_pg_creds` call is a direct consequence of an explicit "Connect" or "Refresh credentials" action.
- **No background work.** No keepalives to OpenBao, no pre-fetching of credentials, no silent token renewal. Expiry is shown; re-fetch is manual.
- **Personal project — don't over-engineer.** Single OpenBao server. Token-based auth via OIDC SSO. One `settings` table row per config key. No multi-user, no namespaces, no HA.
- **Caching over re-fetching.** OpenBao credentials are fetched once per connect and held in `ServerHandle` until expiry (or disconnect). There is no automatic refresh.
- **Pool is a budget, not a default.** The slot manager remains unchanged — credential source has zero impact on slot semantics.

---

## Architecture

```
Quill Settings                 Connection form
  bao_addr + token ----------> credential_source: "password" | "openbao"
         |                        bao_role_path: "database/creds/readonly"
         |                               |
         v                               | connect_server(id, None)
  POST /v1/auth/oidc/          ----------'
  auth_url -> browser SSO      |
  -> localhost callback        v
  -> token in settings     openbao::fetch_pg_creds(role_path)
                               |
                               v
                          PgCredentials { username, password, lease_duration_secs }
                               |
                               v
                          PgConnector -> SlotManager -> Postgres
```

### Data flow per credential source

**`"password"` (unchanged):**
1. User right-clicks server -> "Connect..."
2. Password dialog appears
3. User enters password, submits
4. `api.connectServer(id, password_string)` (string)
5. `connect_server(id, Some(password))` -> builds `PgConnector` with `conn.username` + supplied password
6. `credential_expiry: None`

**`"openbao"` (new):**
1. User right-clicks server -> "Connect..."
2. No dialog — loading spinner on tree node
3. `api.connectServer(id, null)`
4. `connect_server(id, None)` -> loads `OpenBaoClient` from settings -> calls `fetch_pg_creds(bao_role_path)`
5. Uses fetched `username` + `password` (ignores `conn.username`)
6. `credential_expiry: Some(now + lease_duration_secs)`

---

## Current state

Every existing file the implementer must read or modify. Contents are snapshots at time of writing.

### `src-tauri/Cargo.toml` (51 lines)

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
tauri-plugin-dialog = "2"
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
uuid = { version = "1", features = ["v4"] }
rust_decimal = { version = "1", default-features = false, features = ["db-tokio-postgres", "std"] }
sqlparser = "0.55"

[dev-dependencies]
url = "2"
```

### `src-tauri/migrations/0001_initial.sql`

```sql
CREATE TABLE connections (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL UNIQUE,
    host          TEXT    NOT NULL,
    port          INTEGER NOT NULL DEFAULT 5432,
    default_db    TEXT    NOT NULL,
    username      TEXT    NOT NULL,
    ssl_mode      TEXT    NOT NULL DEFAULT 'prefer',
    slot_budget   INTEGER NOT NULL DEFAULT 2,
    password_ref  TEXT,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);
```

### `src-tauri/src/store/mod.rs` (282 lines)

Key types:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Connection {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: i32,
    pub default_db: String,
    pub username: String,
    pub ssl_mode: String,
    pub slot_budget: i32,
    pub password_ref: Option<String>,
    pub created_at: String,
}

pub struct NewConnection {
    pub name: String,
    pub host: String,
    pub port: i32,
    pub default_db: String,
    pub username: String,
    pub ssl_mode: String,
    pub slot_budget: i32,
    pub password_ref: Option<String>,
}
```

Functions: `list()`, `get(id)`, `insert(NewConnection)`, `delete(id)`. No `update`.
Test helper: `sample_new(name)` returns a `NewConnection` with default values.

### `src-tauri/src/commands/mod.rs` (756 lines)

Key elements:

```rust
pub enum CommandError {
    UnknownConnection(String),
    NotConnected(String),
    Slot(String),
    Pg(String),
    Store(String),
    Introspect(String),
    Saved(String),
}

pub async fn connect_server(
    id: i64,
    password: String,                  // <-- will become Option<String>
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<SlotState, CommandError> { ... }

pub async fn save_connection(
    new: store::NewConnection,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<store::Connection, CommandError> { ... }

pub fn get_slot_state(
    server_id: i64,
    registry: State<'_, ServerRegistry>,
) -> Result<Option<SlotState>, CommandError> {
    Ok(registry.by_id.get(&server_id).map(|h| h.slot_manager.state()))
}
```

No `update_connection` command. No `login_openbao`. No `get_openbao_status`.

### `src-tauri/src/slots/mod.rs` (1101 lines)

Relevant public types:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct SlotState {
    pub budget: usize,
    pub slots: Vec<SlotInfo>,
}

pub struct SlotManager<C: Connector> {
    connector: C,
    slots: Mutex<Vec<Slot<C>>>,
    budget: AtomicUsize,
}

impl<C: Connector> SlotManager<C> {
    pub fn new(connector: C, budget: usize) -> Self { ... }

    pub fn state(&self) -> SlotState {
        // Reads budget, locks slots vec, builds SlotState
        SlotState { budget, slots: ... }
    }
}
```

`SlotManager::new(connector, budget)` takes exactly 2 arguments.

### `src-tauri/src/registry.rs` (45 lines)

```rust
pub struct ServerHandle {
    pub slot_manager: Arc<SlotManager<PgConnector>>,
    pub schema_cache: Arc<DashMap<String, SchemaPayload>>,
}

impl ServerHandle {
    pub fn new(connector: PgConnector, budget: usize) -> Self {
        Self {
            slot_manager: Arc::new(SlotManager::new(connector, budget)),
            schema_cache: Arc::new(DashMap::new()),
        }
    }
}
```

### `src-tauri/src/pg/mod.rs` (246 lines)

```rust
pub struct PgConnector {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: SecretString,
    pub ssl_mode: SslPolicy,
}
```

The `PgConnector` is oblivious to credential source — built with whatever username/password we have. No changes needed.

### `src-tauri/src/lib.rs` (55 lines)

Registers all commands in `invoke_handler`. `setup` manages `SqlitePool`, `ServerRegistry`, `ResultRegistry`.

```rust
pub mod commands;
pub mod history;
pub mod introspect;
pub mod parse;
pub mod pg;
pub mod query;
pub mod registry;
pub mod saved;
pub mod slots;
pub mod store;
```

### `src/lib/tauri.ts` (270 lines)

Key types:

```typescript
export type Connection = {
  id: number; name: string; host: string; port: number;
  default_db: string; username: string; ssl_mode: string;
  slot_budget: number; password_ref: string | null; created_at: string;
};

export type NewConnection = {
  name: string; host: string; port: number; default_db: string;
  username: string; ssl_mode: string; slot_budget: number;
  password_ref: null;
};

export type SlotState = { budget: number; slots: SlotInfo[]; };

export type CommandError = {
  kind: "UnknownConnection" | "NotConnected" | "Slot" | "Pg" | "Store"
      | "Introspect" | "Saved";
  message: string;
};

export const api = {
  connectServer: (id: number, password: string) =>
    invoke<SlotState>("connect_server", { id, password }),
  saveConnection: (newConn: NewConnection) =>
    invoke<Connection>("save_connection", { new: newConn }),
  // ... 17 more methods
};
```

No `updateConnection`, no OpenBao methods.

### `src/routes/+page.svelte` (917 lines)

Key elements:

- `addForm: NewConnection` with `defaultAddForm()` returning defaults
- `editingId` does not exist
- `addDialog` for creating connections (no edit mode)
- `pwDialog` for password entry on connect
- `promptPassword(id)` shows password dialog, `submitPassword(e)` calls `api.connectServer(pwTargetId, pwPassword)`
- Context menu: `menuItemsFor(t)` returns items per node kind. Server nodes: "Connect...", "Disconnect", "Copy name", "Delete connection". No "Edit connection...".
- Tree rendering: `onConnectServer={promptPassword}` — always shows password dialog
- No Settings dialog
- No expiry display

### `src/lib/Tree.svelte` (208 lines)

Props: `node, isConnected, selectedDb, onSelectDb, onContextMenu, onConnectServer`.
When a disconnected server is clicked, calls `onConnectServer?.(node.conn.id)`.
Shows loading spinner `node.loading` and error `node.error` for server/database nodes.

No changes needed until Session D (expiry display).

---

## Session A — Migration, structs, edit-connection feature

### A.1 Goal

Before OpenBao exists at all, make connections editable and add the `credential_source` + `bao_role_path` columns. After this session, the app compiles, all existing tests pass, and existing functionality works identically. New columns default to `"password"` / `NULL`. The edit dialog lets users change any field (including the credential source dropdown). Connecting a connection with `credential_source = "openbao"` fails with a clear stub error — OpenBao wiring arrives in Session B.

### A.2 Deliverables

#### A.2.1 `src-tauri/migrations/0004_openbao.sql` (new)

```sql
ALTER TABLE connections ADD COLUMN credential_source TEXT NOT NULL DEFAULT 'password';
ALTER TABLE connections ADD COLUMN bao_role_path TEXT;

CREATE TABLE settings (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);
```

#### A.2.2 `src-tauri/src/store/mod.rs` (modified)

Add two fields to both structs (after `password_ref`, before `created_at`):

```rust
pub struct Connection {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: i32,
    pub default_db: String,
    pub username: String,
    pub ssl_mode: String,
    pub slot_budget: i32,
    pub password_ref: Option<String>,
    pub credential_source: String,        // "password" | "openbao"
    pub bao_role_path: Option<String>,    // e.g. "database/creds/readonly"
    pub created_at: String,
}

pub struct NewConnection {
    pub name: String,
    pub host: String,
    pub port: i32,
    pub default_db: String,
    pub username: String,
    pub ssl_mode: String,
    pub slot_budget: i32,
    pub password_ref: Option<String>,
    pub credential_source: String,
    pub bao_role_path: Option<String>,
}
```

Updated SQL in `list()`:

```rust
"SELECT id, name, host, port, default_db, username, ssl_mode, slot_budget, \
        password_ref, credential_source, bao_role_path, created_at \
 FROM connections \
 ORDER BY name"
```

Updated SQL in `get()` — same column list as `list()`.

Updated `insert()`:

```rust
"INSERT INTO connections (name, host, port, default_db, username, ssl_mode, \
                          slot_budget, password_ref, credential_source, bao_role_path) \
 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
 RETURNING id, name, host, port, default_db, username, ssl_mode, slot_budget, \
           password_ref, credential_source, bao_role_path, created_at"
```

Bindings: add `.bind(&c.credential_source)` and `.bind(&c.bao_role_path)`.

New `update()` function:

```rust
pub async fn update(
    pool: &SqlitePool,
    id: i64,
    c: NewConnection,
) -> Result<Connection, StoreError> {
    Ok(sqlx::query_as::<_, Connection>(
        "UPDATE connections SET \
            name = ?, host = ?, port = ?, default_db = ?, username = ?, \
            ssl_mode = ?, slot_budget = ?, password_ref = ?, \
            credential_source = ?, bao_role_path = ? \
         WHERE id = ? \
         RETURNING id, name, host, port, default_db, username, ssl_mode, \
                    slot_budget, password_ref, credential_source, bao_role_path, created_at",
    )
    .bind(&c.name)
    .bind(&c.host)
    .bind(c.port)
    .bind(&c.default_db)
    .bind(&c.username)
    .bind(&c.ssl_mode)
    .bind(c.slot_budget)
    .bind(&c.password_ref)
    .bind(&c.credential_source)
    .bind(&c.bao_role_path)
    .bind(id)
    .fetch_one(pool)
    .await?)
}
```

Update `sample_new()`:

```rust
fn sample_new(name: &str) -> NewConnection {
    NewConnection {
        // ... existing fields unchanged ...
        credential_source: "password".into(),
        bao_role_path: None,
    }
}
```

#### A.2.3 `src-tauri/src/commands/mod.rs` (modified)

Add variant to `CommandError`:

```rust
pub enum CommandError {
    UnknownConnection(String),
    NotConnected(String),
    Slot(String),
    Pg(String),
    Store(String),
    Introspect(String),
    Saved(String),
    OpenBao(String),
}
```

Update `Display` impl to include `Self::OpenBao(msg) => write!(f, "{msg}")`.

Add `From<OpenBaoError>` — type doesn't exist yet, so add a placeholder comment or leave it for Session B. The variant itself is enough.

**New `update_connection` command:**

```rust
#[tauri::command]
pub async fn update_connection(
    id: i64,
    new: store::NewConnection,
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<store::Connection, CommandError> {
    if registry.by_id.contains_key(&id) {
        return Err(CommandError::OpenBao(
            "Disconnect the server before editing its configuration.".into(),
        ));
    }
    Ok(store::update(&pool, id, new).await?)
}
```

**Update `connect_server` signature + logic:**

```rust
pub async fn connect_server(
    id: i64,
    password: Option<String>,               // CHANGED: was String
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<SlotState, CommandError> {
    if let Some(handle) = registry.by_id.get(&id) {
        return Ok(handle.slot_manager.state());
    }

    let conn = store::get(&pool, id)
        .await?
        .ok_or_else(|| CommandError::unknown_connection(id))?;

    if conn.credential_source == "openbao" {
        return Err(CommandError::OpenBao(
            "OpenBao credential source is not yet implemented.".into(),
        ));
    }

    let password = password.ok_or_else(|| {
        CommandError::Pg("password is required for password-based connections".into())
    })?;

    let ssl_mode = PgConnector::parse_ssl_mode(&conn.ssl_mode)
        .map_err(|e| CommandError::Pg(e.0))?;
    let connector = PgConnector {
        host: conn.host.clone(),
        port: conn.port as u16,
        username: conn.username.clone(),
        password: SecretString::from(password),
        ssl_mode,
    };

    let budget = conn.slot_budget.max(1) as usize;
    let handle = ServerHandle::new(connector, budget);
    let state = handle.slot_manager.state();
    registry.by_id.insert(id, handle);
    Ok(state)
}
```

#### A.2.4 `src-tauri/src/lib.rs` (modified)

Add to module declarations: `pub mod openbao;` (will be empty stub until Session B).

Register `update_connection`:

```rust
.invoke_handler(tauri::generate_handler![
    commands::list_connections,
    commands::save_connection,
    commands::update_connection,       // NEW
    commands::delete_connection,
    commands::connect_server,
    // ... rest unchanged
])
```

#### A.2.5 `src/lib/tauri.ts` (modified)

```typescript
export type CredentialSource = "password" | "openbao";

export type Connection = {
  id: number; name: string; host: string; port: number;
  default_db: string; username: string; ssl_mode: string;
  slot_budget: number; password_ref: string | null;
  credential_source: string;           // NEW
  bao_role_path: string | null;        // NEW
  created_at: string;
};

export type NewConnection = {
  name: string; host: string; port: number; default_db: string;
  username: string; ssl_mode: string; slot_budget: number;
  password_ref: null;
  credential_source: string;           // NEW
  bao_role_path: string | null;        // NEW
};

export type CommandError = {
  kind: "UnknownConnection" | "NotConnected" | "Slot" | "Pg"
      | "Store" | "Introspect" | "Saved" | "OpenBao";
  message: string;
};

export const api = {
  // ... all existing methods ...

  connectServer: (id: number, password: string | null) =>     // CHANGED
    invoke<SlotState>("connect_server", { id, password }),

  updateConnection: (id: number, update: NewConnection) =>    // NEW
    invoke<Connection>("update_connection", { id, new: update }),
};
```

#### A.2.6 `src/routes/+page.svelte` (modified)

**New state variable:**

```typescript
let editingId = $state<number | null>(null);
```

**Update `defaultAddForm()`:**

```typescript
function defaultAddForm(): NewConnection {
  return {
    name: "", host: "localhost", port: 5432, default_db: "postgres",
    username: "postgres", ssl_mode: "disable", slot_budget: 2,
    password_ref: null,
    credential_source: "password",     // NEW
    bao_role_path: null,               // NEW
  };
}
```

**New `openEditModal` function:**

```typescript
function openEditModal(conn: Connection) {
  editingId = conn.id;
  addForm = {
    name: conn.name, host: conn.host, port: conn.port,
    default_db: conn.default_db, username: conn.username,
    ssl_mode: conn.ssl_mode, slot_budget: conn.slot_budget,
    password_ref: null,
    credential_source: conn.credential_source,
    bao_role_path: conn.bao_role_path,
  };
  addError = "";
  addDialog?.showModal();
}
```

**Update `saveConnection` to route between insert and update:**

```typescript
async function saveConnection(e: Event) {
  e.preventDefault();
  addError = "";
  try {
    if (editingId !== null) {
      await api.updateConnection(editingId, { ...addForm });
    } else {
      await api.saveConnection({ ...addForm });
    }
    const rows = await api.listConnections();
    connections = rows;
    tree = rows.map(makeServerNode);
    addDialog?.close();
    editingId = null;
  } catch (err) {
    addError = errorMessage(err);
  }
}
```

**Modify connection form dialog:**

- Title: `{editingId !== null ? "Edit Connection" : "Add Connection"}`
- After Username field, add credential source dropdown:

```svelte
<label class="field">
  Credential source
  <select class="input" bind:value={addForm.credential_source}>
    <option value="password">Password</option>
    <option value="openbao">OpenBao</option>
  </select>
</label>
```

- After SSL mode block, conditionally show role path:

```svelte
{#if addForm.credential_source === "openbao"}
  <label class="field">
    Role path
    <input class="input" bind:value={addForm.bao_role_path}
           placeholder="database/creds/my-role" />
  </label>
{/if}
```

**Update context menu (`menuItemsFor`):**

```typescript
function menuItemsFor(t: TreeNode): { action: string; label: string }[] {
  const items: { action: string; label: string }[] = [];
  if (t.kind === "server") {
    if (isConnected(t.conn.id)) {
      items.push({ action: "disconnect", label: "Disconnect" });
    } else {
      items.push({ action: "connect", label: "Connect..." });
      items.push({ action: "edit", label: "Edit connection..." });  // NEW
    }
    items.push({ action: "copy-name", label: "Copy name" });
    if (!isConnected(t.conn.id)) {
      items.push({ action: "delete", label: "Delete connection" });
    }
  } else if (t.kind === "database") {
    // ... unchanged
  }
  return items;
}
```

**Update `menuAction`:**

```typescript
case "edit":
  if (target.kind === "server") openEditModal(target.conn);
  break;
```

**Update `disconnect` — clear editingId if the disconnected server was being edited:**

```typescript
async function disconnect(id: number) {
  // ... existing cleanup ...
  await api.disconnectServer(id);
  // ... existing state cleanup ...
  if (editingId === id) editingId = null;
  // ... rest unchanged
}
```

### A.3 Implementation order

1. Create `src-tauri/migrations/0004_openbao.sql`
2. Add `credential_source` + `bao_role_path` to `Connection` / `NewConnection` structs
3. Update `list`, `get`, `insert` SQL to include new columns
4. Add `store::update()` function
5. Update `sample_new()` test helper
6. Add `CommandError::OpenBao` variant + Display/From impls
7. Add `update_connection` command
8. Change `connect_server` signature to `password: Option<String>` + OpenBao stub
9. Register `update_connection` in `lib.rs`; add `pub mod openbao;` as empty stub
10. Update `src/lib/tauri.ts` types + new api methods
11. Update `+page.svelte`: `editingId`, `openEditModal`, form changes, context menu
12. Run `./test.sh` — must pass

### A.4 Gotchas

- **sqlx `FromRow` column order.** The `RETURNING` clause must list columns in the same order as the struct fields. Mismatch causes silent panics. Verify with the test suite.
- **SQLite `ALTER TABLE ADD COLUMN ... DEFAULT 'password'`** fills existing rows. Old code using `SELECT *` would break when struct fields are added — but our `list`/`get`/`insert` explicitly list every column, so this is safe.
- **Unique constraint on name.** Editing a connection's name to an existing one must produce a readable error. The existing `StoreError::Sqlx` + `db_err.is_unique_violation()` test path handles this.
- **Edit-while-connected.** The `update_connection` command blocks if the server is in the registry. The frontend also hides "Edit connection..." when connected. Both checks exist; the backend check is authoritative.
- **`onConnectServer` prop.** Currently `Tree.svelte` calls `onConnectServer?.(node.conn.id)` when a disconnected server is clicked. In Session A, this still maps to `promptPassword` (unchanged binding). In Session C, it becomes a routing function.

### A.5 Test cases

1. **Migration on fresh DB.** In-memory SQLite, run migrations, verify `credential_source` column exists with default `'password'`.
2. **Migration on existing DB.** Insert a connection using old schema, run migration, verify row now has `credential_source = 'password'` and `bao_role_path IS NULL`.
3. **Insert with `credential_source = 'openbao'`.** Insert with explicit role path, fetch back, verify both fields.
4. **Update changes credential_source.** Insert with `'password'`, update to `'openbao'` with `bao_role_path`, fetch back, verify.
5. **Update blocked while connected.** Connect a server, attempt edit via `update_connection` command -> error "Disconnect the server before editing".
6. **Edit form pre-fills correctly.** Manual: save a connection, right-click -> Edit, verify all fields match original values.
7. **Credential source dropdown toggles fields.** Manual: edit form, switch between Password and OpenBao, verify Role path appears/hides.
8. **Connect with `"openbao"` source returns stub error.** Manual: create connection with `credential_source = 'openbao'`, connect -> error "OpenBao credential source is not yet implemented."

### A.6 Acceptance criteria

- `./test.sh` passes (cargo test + svelte-check)
- `./run.sh` starts, the app renders
- "password" connections connect and run queries identically to before
- New "openbao" connections show the stub error on connect
- Edit form opens pre-filled, saves changes, reflects them in the tree
- Edit is hidden in context menu when connected; backend blocks if somehow reached

---

## Session B — OpenBao backend

### B.1 Goal

Implement the Rust-side OpenBao integration. After this session the app can:
- Store and retrieve `openbao_addr` and `openbao_token` in the `settings` table
- Start an OIDC browser flow, capture the callback, and persist the token
- Fetch dynamic Postgres credentials from an OpenBao role path
- Route `connect_server` by credential source: OpenBao connections fetch credentials and connect to Postgres
- Return `credential_expiry` in `SlotState` for OpenBao connections

### B.2 Deliverables

#### B.2.1 `src-tauri/Cargo.toml` (modified)

Add under `[dependencies]`:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

Move `url` from `[dev-dependencies]` to `[dependencies]`:

```toml
url = "2"
```

Remove `url` from `[dev-dependencies]`. Verify no other dev-dependency uses it.

#### B.2.2 `src-tauri/src/openbao.rs` (new)

```rust
use std::time::{Duration, SystemTime};

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use thiserror::Error;

// ── Error type ──

#[derive(Debug, Error)]
pub enum OpenBaoError {
    #[error("OpenBao not configured — set the server address in Settings")]
    NotConfigured,
    #[error("no OpenBao token — login in Settings")]
    NoToken,
    #[error("OpenBao request failed: {0}")]
    Request(String),
    #[error("OpenBao returned unexpected response: {0}")]
    BadResponse(String),
    #[error("OIDC login timed out")]
    LoginTimeout,
    #[error("OIDC callback failed: {0}")]
    LoginCallback(String),
    #[error("settings error: {0}")]
    Store(String),
}

impl From<reqwest::Error> for OpenBaoError {
    fn from(e: reqwest::Error) -> Self {
        Self::Request(e.to_string())
    }
}

impl From<sqlx::Error> for OpenBaoError {
    fn from(e: sqlx::Error) -> Self {
        Self::Store(e.to_string())
    }
}

// ── Response types (private, used during deserialization) ──

#[derive(Debug, Deserialize)]
struct OidcAuthUrlResponse {
    data: OidcAuthUrlData,
}

#[derive(Debug, Deserialize)]
struct OidcAuthUrlData {
    auth_url: String,
}

#[derive(Debug, Deserialize)]
struct OidcCallbackResponse {
    auth: OidcCallbackAuth,
}

#[derive(Debug, Deserialize)]
struct OidcCallbackAuth {
    client_token: String,
}

#[derive(Debug, Deserialize)]
struct PgCredsResponse {
    data: PgCredsData,
}

#[derive(Debug, Deserialize)]
struct PgCredsData {
    username: String,
    password: String,
    #[serde(default)]
    lease_duration: u64,
}

// ── Public types ──

pub struct PgCredentials {
    pub username: String,
    pub password: SecretString,
    pub lease_duration_secs: u64,
}

pub struct OpenBaoClient {
    pub addr: String,
    token: SecretString,
    client: reqwest::Client,
}

// ── Settings CRUD (free functions) ──

pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<(), OpenBaoError> {
    sqlx::query(
        "INSERT INTO settings(key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_setting(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

pub async fn remove_setting(pool: &SqlitePool, key: &str) -> Result<(), OpenBaoError> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

// ── OpenBaoClient ──

impl OpenBaoClient {
    pub async fn from_store(pool: &SqlitePool) -> Result<Option<Self>, OpenBaoError> {
        let addr = match get_setting(pool, "openbao_addr").await {
            Some(a) => a,
            None => return Ok(None),
        };
        let token = match get_setting(pool, "openbao_token").await {
            Some(t) => t,
            None => return Ok(None),
        };
        Ok(Some(Self {
            addr,
            token: SecretString::from(token),
            client: reqwest::Client::new(),
        }))
    }

    pub async fn fetch_pg_creds(&self, role_path: &str) -> Result<PgCredentials, OpenBaoError> {
        let url = format!("{}/v1/{}", self.addr.trim_end_matches('/'), role_path);

        let resp = self
            .client
            .get(&url)
            .header("X-Vault-Token", self.token.expose_secret())
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(OpenBaoError::Request(format!(
                "OpenBao returned {}: {}",
                resp.status(),
                body,
            )));
        }

        let creds: PgCredsResponse = resp
            .json()
            .await
            .map_err(|e| OpenBaoError::BadResponse(format!("failed to parse credentials: {e}")))?;

        Ok(PgCredentials {
            username: creds.data.username,
            password: SecretString::from(creds.data.password),
            lease_duration_secs: if creds.data.lease_duration > 0 {
                creds.data.lease_duration
            } else {
                3600
            },
        })
    }
}

// ── OIDC flow ──

pub async fn start_oidc_login(
    bao_addr: &str,
    app_handle: &tauri::AppHandle,
) -> Result<String, OpenBaoError> {
    use tauri::Manager;

    let client = reqwest::Client::new();

    // 1. Bind local TCP listener
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| OpenBaoError::LoginCallback(format!("bind failed: {e}")))?;
    let port = listener.local_addr().unwrap().port();
    let redirect_uri = format!("http://localhost:{port}/callback");

    // 2. Query OpenBao for the OIDC auth URL
    let auth_url_resp: OidcAuthUrlResponse = client
        .post(format!(
            "{}/v1/auth/oidc/auth_url",
            bao_addr.trim_end_matches('/')
        ))
        .json(&serde_json::json!({
            "role": "default",
            "redirect_uri": &redirect_uri,
        }))
        .send()
        .await?
        .json()
        .await
        .map_err(|e| OpenBaoError::BadResponse(format!("auth_url parse: {e}")))?;

    // 3. Open browser
    app_handle
        .shell()
        .open(&auth_url_resp.data.auth_url, None)
        .map_err(|e| OpenBaoError::LoginCallback(format!("browser open failed: {e}")))?;

    // 4. Wait for callback with 5-minute timeout
    let (code, state) = accept_callback(listener).await?;

    // 5. Exchange code+state for token
    let cb_resp: OidcCallbackResponse = client
        .post(format!(
            "{}/v1/auth/oidc/callback",
            bao_addr.trim_end_matches('/')
        ))
        .json(&serde_json::json!({
            "state": state,
            "code": code,
        }))
        .send()
        .await?
        .json()
        .await
        .map_err(|e| OpenBaoError::BadResponse(format!("callback parse: {e}")))?;

    Ok(cb_resp.auth.client_token)
}

async fn accept_callback(
    listener: std::net::TcpListener,
) -> Result<(String, String), OpenBaoError> {
    listener
        .set_nonblocking(true)
        .map_err(|e| OpenBaoError::LoginCallback(e.to_string()))?;
    let tokio_listener = tokio::net::TcpListener::from_std(listener)
        .map_err(|e| OpenBaoError::LoginCallback(e.to_string()))?;

    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(300), tokio_listener.accept())
        .await
        .map_err(|_| OpenBaoError::LoginTimeout)?
        .map_err(|e| OpenBaoError::LoginCallback(e.to_string()))?;

    let (reader, mut writer) = stream.split();
    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut request_line = String::new();
    buf_reader
        .read_line(&mut request_line)
        .await
        .map_err(|e| OpenBaoError::LoginCallback(e.to_string()))?;

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(OpenBaoError::LoginCallback("malformed request".into()));
    }
    let path = parts[1];

    let parsed = url::Url::parse(&format!("http://localhost{path}"))
        .map_err(|_| OpenBaoError::LoginCallback("malformed URL".into()))?;

    let code = parsed
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| OpenBaoError::LoginCallback("missing 'code' parameter".into()))?;

    let state = parsed
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| OpenBaoError::LoginCallback("missing 'state' parameter".into()))?;

    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
                    <html><body><h1>Quill</h1><p>Login successful. \
                    You can close this tab.</p></body></html>";
    writer
        .write_all(response.as_bytes())
        .await
        .map_err(|e| OpenBaoError::LoginCallback(e.to_string()))?;

    Ok((code, state))
}
```

#### B.2.3 `src-tauri/src/slots/mod.rs` (modified)

Add `credential_expiry` to `SlotState` only — **no changes to `SlotManager`**:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct SlotState {
    pub budget: usize,
    pub slots: Vec<SlotInfo>,
    pub credential_expiry: Option<SystemTime>,   // NEW
}
```

`SlotManager` struct and `SlotManager::new(connector, budget)` stay exactly as-is. `SlotManager::state()` stays as-is (returns `SlotState { budget, slots, credential_expiry: None }` — expiry is set by the caller, see B.2.5).

#### B.2.4 `src-tauri/src/registry.rs` (modified)

Add `credential_expiry` to `ServerHandle` — `SlotManager` stays unchanged:

```rust
pub struct ServerHandle {
    pub slot_manager: Arc<SlotManager<PgConnector>>,
    pub schema_cache: Arc<DashMap<String, SchemaPayload>>,
    pub credential_expiry: Option<SystemTime>,   // NEW
}

impl ServerHandle {
    pub fn new(connector: PgConnector, budget: usize) -> Self {
        Self {
            slot_manager: Arc::new(SlotManager::new(connector, budget)),  // unchanged
            schema_cache: Arc::new(DashMap::new()),
            credential_expiry: None,
        }
    }
}
```

#### B.2.5 `src-tauri/src/commands/mod.rs` (modified)

**Add imports:**

```rust
use crate::openbao;
use std::time::{SystemTime, Duration};
```

**Add `From<OpenBaoError>`:**

```rust
impl From<openbao::OpenBaoError> for CommandError {
    fn from(e: openbao::OpenBaoError) -> Self {
        Self::OpenBao(e.to_string())
    }
}
```

**New command — `login_openbao`:**

```rust
#[tauri::command]
pub async fn login_openbao(
    pool: State<'_, sqlx::SqlitePool>,
    app: tauri::AppHandle,
) -> Result<String, CommandError> {
    let addr = openbao::get_setting(&pool, "openbao_addr")
        .await
        .ok_or_else(|| {
            CommandError::OpenBao(
                "OpenBao address not configured. Set it in Settings.".into(),
            )
        })?;

    let token = openbao::start_oidc_login(&addr, &app).await?;
    openbao::set_setting(&pool, "openbao_token", &token).await?;
    Ok("Login successful.".into())
}
```

**New command — `get_openbao_status`:**

```rust
#[derive(Debug, Serialize)]
pub struct OpenBaoStatus {
    pub configured: bool,
    pub has_token: bool,
    pub addr: Option<String>,
}

#[tauri::command]
pub async fn get_openbao_status(
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<OpenBaoStatus, CommandError> {
    let addr = openbao::get_setting(&pool, "openbao_addr").await;
    let token = openbao::get_setting(&pool, "openbao_token").await;
    Ok(OpenBaoStatus {
        configured: addr.is_some(),
        has_token: token.is_some(),
        addr,
    })
}
```

**New command — `clear_openbao_token`:**

```rust
#[tauri::command]
pub async fn clear_openbao_token(
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<(), CommandError> {
    openbao::remove_setting(&pool, "openbao_token").await?;
    Ok(())
}
```

**New command — `set_setting` (generic, needed by Settings UI):**

```rust
#[tauri::command]
pub async fn set_setting(
    key: String,
    value: String,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<(), CommandError> {
    openbao::set_setting(&pool, &key, &value).await?;
    Ok(())
}
```

**Replace `connect_server` with full routing:**

```rust
#[tauri::command]
pub async fn connect_server(
    id: i64,
    password: Option<String>,
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
) -> Result<SlotState, CommandError> {
    if let Some(handle) = registry.by_id.get(&id) {
        let mut state = handle.slot_manager.state();
        state.credential_expiry = handle.credential_expiry;
        return Ok(state);
    }

    let conn = store::get(&pool, id)
        .await?
        .ok_or_else(|| CommandError::unknown_connection(id))?;

    let (username, password, expiry) = match conn.credential_source.as_str() {
        "password" => {
            let pw = password.ok_or_else(|| {
                CommandError::Pg("password is required for password-based connections".into())
            })?;
            (conn.username.clone(), SecretString::from(pw), None)
        }
        "openbao" => {
            let bao = openbao::OpenBaoClient::from_store(&pool)
                .await?
                .ok_or_else(|| {
                    CommandError::OpenBao(
                        "OpenBao not configured. Set up the server address and login in Settings."
                            .into(),
                    )
                })?;
            let role_path = conn.bao_role_path.as_deref().ok_or_else(|| {
                CommandError::OpenBao("no role path configured for this connection".into())
            })?;
            let creds = bao.fetch_pg_creds(role_path).await?;
            let expiry =
                SystemTime::now().checked_add(Duration::from_secs(creds.lease_duration_secs));
            (creds.username, creds.password, expiry)
        }
        other => {
            return Err(CommandError::Pg(format!(
                "unknown credential_source: {other}"
            )));
        }
    };

    let ssl_mode =
        PgConnector::parse_ssl_mode(&conn.ssl_mode).map_err(|e| CommandError::Pg(e.0))?;
    let connector = PgConnector {
        host: conn.host.clone(),
        port: conn.port as u16,
        username,
        password,
        ssl_mode,
    };

    let budget = conn.slot_budget.max(1) as usize;
    let mut handle = ServerHandle::new(connector, budget);
    handle.credential_expiry = expiry;
    let state = handle.slot_manager.state();
    registry.by_id.insert(id, handle);
    Ok(state)
}
```

**Update `get_slot_state` to include `credential_expiry`:**

```rust
pub fn get_slot_state(
    server_id: i64,
    registry: State<'_, ServerRegistry>,
) -> Result<Option<SlotState>, CommandError> {
    Ok(registry.by_id.get(&server_id).map(|h| {
        let mut state = h.slot_manager.state();
        state.credential_expiry = h.credential_expiry;
        state
    }))
}
```

#### B.2.6 `src-tauri/src/lib.rs` (modified)

Update `pub mod openbao;` from empty stub to real module (remove any placeholder).

Register new commands:

```rust
commands::update_connection,
commands::login_openbao,          // NEW
commands::get_openbao_status,     // NEW
commands::clear_openbao_token,    // NEW
commands::set_setting,            // NEW
```

### B.3 Implementation order

1. Add `reqwest` + move `url` in `Cargo.toml`; run `cargo check` to fetch deps
2. Create `src-tauri/src/openbao.rs` with error type, response types, settings helpers, `OpenBaoClient`, OIDC flow
3. Add `credential_expiry: Option<SystemTime>` to `SlotState` in `slots/mod.rs`
4. Add `credential_expiry: Option<SystemTime>` to `ServerHandle` in `registry.rs`
5. Add `From<OpenBaoError>` + new commands to `commands/mod.rs`
6. Rewrite `connect_server` with full routing
7. Update `get_slot_state` to add `credential_expiry`
8. Register everything in `lib.rs`
9. Run `cargo test && cargo clippy -- -D warnings`

### B.4 Gotchas

- **`tauri::Manager` import.** `start_oidc_login` calls `app_handle.shell().open(...)`, which needs `use tauri::Manager;`.
- **`TcpListener` conversion.** Use `listener.set_nonblocking(true)` then `tokio::net::TcpListener::from_std()`. This avoids blocking the async runtime.
- **OIDC role hardcoded.** `"role": "default"` is hardcoded for v1. The OIDC role name in OpenBao is separate from the DB role path.
- **`SystemTime` serialization.** Already serialized by serde as `{ secs_since_epoch, nanos_since_epoch }`. The frontend already handles this for `SlotInfo.last_used`. No custom serialize needed.
- **`connect_server` already-connected path.** When the server is already in the registry, `credential_expiry` is read from `ServerHandle` and set on the returned `SlotState`. The `SlotManager` doesn't know about it — the field is set after `state()` returns.
- **`SlotManager::new` unchanged.** All 17 call sites remain untouched; no test rewrites.

### B.5 Test cases

1. **`set_setting` + `get_setting` round-trip.** Use in-memory SQLite pool, write a value, read it back.
2. **`remove_setting`.** Write then remove, verify gone.
3. **`from_store` returns `None` when config missing.** No addr/token in settings -> `from_store` returns `Ok(None)`.
4. **`from_store` returns `Some` when configured.** Set addr + token -> returns `Ok(Some(client))`.
5. **`fetch_pg_creds` with mock.** Manual test with a local `bao dev` or Vault dev instance.
6. **`start_oidc_login` end-to-end.** Manual with real OpenBao: start SSO, complete in browser, verify token persisted.
7. **Connect via OpenBao.** Manual: configure OpenBao + login + create connection with `"openbao"` + role path -> connect -> run query.
8. **Connect via OpenBao, no token.** Delete token, try connect -> error "OpenBao not configured".
9. **Connect via OpenBao, wrong role path.** -> error from OpenBao API.
10. **`SlotState.credential_expiry` present.** Connect via OpenBao, call `get_slot_state`, verify `credential_expiry` is `Some` and in the future.
11. **`SlotState.credential_expiry` absent for password.** Connect via password, verify `credential_expiry` is `None`.

### B.6 Acceptance criteria

- `cargo test` passes, `cargo clippy -- -D warnings` passes
- `cargo build` succeeds
- `./test.sh` passes
- "password" connections connect and query identically to before
- "openbao" connections fetch credentials and query Postgres
- `get_openbao_status` returns correct state
- OIDC login opens browser, captures token on callback

---

## Session C — Frontend wire-up

### C.1 Goal

Replace the Session A stub with full UI support. After this session the user can:
- Set the OpenBao address and login via browser SSO from a Settings dialog
- Create and edit connections with `credential_source = "openbao"` + role path
- Connect to OpenBao-backed servers with one click (no password dialog)
- See connection errors surfaced inline on the server tree node

### C.2 Deliverables

#### C.2.1 `src/lib/tauri.ts` (modified)

Add `OpenBaoStatus` type and new API methods:

```typescript
export type OpenBaoStatus = {
  configured: boolean;
  has_token: boolean;
  addr: string | null;
};

export const api = {
  // ... all existing methods, including those from Session A ...

  // OpenBao (Sessions B+C)
  loginOpenBao: () =>
    invoke<string>("login_openbao"),

  getOpenBaoStatus: () =>
    invoke<OpenBaoStatus>("get_openbao_status"),

  clearOpenBaoToken: () =>
    invoke<void>("clear_openbao_token"),

  setSetting: (key: string, value: string) =>
    invoke<void>("set_setting", { key, value }),
};
```

#### C.2.2 `src/routes/+page.svelte` (modified)

**New state variables for Settings dialog:**

```typescript
let settingsDialog = $state<HTMLDialogElement | null>(null);
let baoAddr = $state("");
let baoHasToken = $state(false);
let baoStatusError = $state("");
let baoLoginBusy = $state(false);
```

**New functions:**

```typescript
async function refreshOpenBaoStatus() {
  try {
    const status = await api.getOpenBaoStatus();
    baoAddr = status.addr ?? "";
    baoHasToken = status.has_token;
    baoStatusError = "";
  } catch (err) {
    baoStatusError = errorMessage(err);
  }
}

async function saveBaoAddr() {
  baoStatusError = "";
  try {
    await api.setSetting("openbao_addr", baoAddr);
    await refreshOpenBaoStatus();
  } catch (err) {
    baoStatusError = errorMessage(err);
  }
}

async function loginOpenBao() {
  baoStatusError = "";
  baoLoginBusy = true;
  try {
    const msg = await api.loginOpenBao();
    await refreshOpenBaoStatus();
    baoStatusError = msg;
  } catch (err) {
    baoStatusError = errorMessage(err);
  } finally {
    baoLoginBusy = false;
  }
}

async function clearOpenBaoToken() {
  baoStatusError = "";
  try {
    await api.clearOpenBaoToken();
    await refreshOpenBaoStatus();
  } catch (err) {
    baoStatusError = errorMessage(err);
  }
}

function openSettings() {
  refreshOpenBaoStatus();
  settingsDialog?.showModal();
}
```

**New connect routing — replace `onConnectServer={promptPassword}` with `connectServer`:**

```typescript
function connectServer(id: number) {
  const conn = connections.find(c => c.id === id);
  if (!conn) return;
  if (conn.credential_source === "openbao") {
    connectViaOpenBao(id);
  } else {
    promptPassword(id);
  }
}

async function connectViaOpenBao(id: number) {
  const serverNode = tree.find(n => n.conn.id === id);
  if (!serverNode) return;
  serverNode.loading = true;
  serverNode.error = null;
  try {
    const state = await api.connectServer(id, null);
    connectedState[id] = state;
    serverNode.expanded = true;
    loadDatabases(serverNode);
  } catch (err) {
    serverNode.error = errorMessage(err);
  } finally {
    serverNode.loading = false;
  }
}
```

**Update the tree's `onConnectServer` binding** (line ~641):

Change from:
```svelte
onConnectServer={promptPassword}
```

To:
```svelte
onConnectServer={connectServer}
```

**Settings dialog markup** (add after password dialog, before change-database dialog):

```svelte
<!-- ═══════ SETTINGS DIALOG ═══════ -->
<dialog bind:this={settingsDialog} class="modal">
  <h2>Settings</h2>
  <div class="add-form">
    <h3>OpenBao</h3>
    <label class="field">
      Server address
      <input class="input"
             bind:value={baoAddr}
             placeholder="https://vault.internal:8200" />
    </label>
    <button class="btn btn-primary" onclick={saveBaoAddr}>Save address</button>

    <p style="margin-top: 1rem;">
      Token: <strong>{baoHasToken ? "Present" : "None"}</strong>
    </p>
    <div class="modal-actions" style="justify-content: flex-start;">
      <button class="btn btn-primary" onclick={loginOpenBao} disabled={baoLoginBusy}>
        {baoLoginBusy ? "Opening browser…" : "Login with browser"}
      </button>
      {#if baoHasToken}
        <button class="btn" onclick={clearOpenBaoToken}>Clear token</button>
      {/if}
    </div>

    {#if baoStatusError}
      <p class="error">{baoStatusError}</p>
    {/if}

    <div class="modal-actions" style="margin-top: 1rem;">
      <button type="button" class="btn" onclick={() => settingsDialog?.close()}>Close</button>
    </div>
  </div>
</dialog>
```

**Add Settings button** to left pane header (line ~623):

```svelte
<div class="header-row">
  <h2>Connections</h2>
  <div style="display: flex; gap: 0.3rem;">
    <button class="btn" onclick={openSettings} title="Settings">⚙</button>
    <button class="btn" onclick={openAddModal}>+ Add</button>
  </div>
</div>
```

### C.3 Implementation order

1. Add `OpenBaoStatus` type + new API methods to `tauri.ts`
2. Add Settings state variables (`settingsDialog`, `baoAddr`, etc.) to `+page.svelte`
3. Add Settings functions to `+page.svelte`
4. Add `connectServer` + `connectViaOpenBao` routing functions
5. Change `onConnectServer={promptPassword}` to `onConnectServer={connectServer}` in tree render
6. Add Settings dialog markup (after password dialog in template)
7. Add Settings button to left pane header
8. Run `./test.sh`

### C.4 Gotchas

- **`connectServer` name shadow.** The helper function `connectServer(id)` shadows `api.connectServer`. Inside the helper, use `api.connectServer(id, pw)` explicitly.
- **`tree.find` on `$state` array.** `ServerNode[]` is a plain array; `.find()` works synchronously.
- **`serverNode.loading` and `serverNode.error`.** The `ServerNode` type (from `$lib/tree`) already has `loading: bool` and `error: string | null`. The `Tree.svelte` component already renders them.
- **Saved settings persistence.** `set_setting` upserts into the `settings` table. No additional migration needed beyond 0004.
- **`tauri-plugin-opener` not used in frontend.** The browser is opened from Rust (`start_oidc_login` calls `app_handle.shell().open(...)`). The frontend only calls `api.loginOpenBao()`.

### C.5 Test cases (manual)

1. Open Settings. Set OpenBao address, save, close, reopen — address persists.
2. "Login with browser" — browser opens with IdP. Complete SSO. Settings shows "Token: Present".
3. Clear token — "Token: None".
4. Create connection with `credential_source = "openbao"` + valid role path. Connect — tree expands, databases load. Run query.
5. Create connection with `"openbao"` but no token — error on server node.
6. Create connection with `"openbao"` but no role path — error.
7. Create connection with `"password"` — password dialog appears, connects normally.
8. Edit a "password" connection to "openbao", don't fill role path. Connect — error about missing role path.

### C.6 Acceptance criteria

- `./test.sh` passes
- Settings dialog: save address, login, clear token all work
- "password" connections work exactly as before
- "openbao" connections connect with one click, fetch credentials, allow queries
- Errors from OpenBao (no token, wrong path, network error) show on server node

---

## Session D — Expiry warnings and polish

### D.1 Goal

Add credential-expiry indicators to the tree, the "Refresh OpenBao credentials" context menu action, and polish edge cases. After this session, the feature is complete.

### D.2 Deliverables

#### D.2.1 `src-tauri/src/commands/mod.rs` (modified)

**New command — `refresh_openbao_creds`:**

```rust
#[tauri::command]
pub async fn refresh_openbao_creds(
    id: i64,
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
    results: State<'_, ResultRegistry>,
) -> Result<SlotState, CommandError> {
    if let Some((_, handle)) = registry.by_id.remove(&id) {
        query::sweep_for_server(id, &results).await;
        handle.slot_manager.disconnect_all();
        drop(handle);
    }

    let conn = store::get(&pool, id)
        .await?
        .ok_or_else(|| CommandError::unknown_connection(id))?;

    if conn.credential_source != "openbao" {
        return Err(CommandError::OpenBao(
            "This server does not use OpenBao credentials.".into(),
        ));
    }

    let bao = openbao::OpenBaoClient::from_store(&pool)
        .await?
        .ok_or_else(|| {
            CommandError::OpenBao(
                "OpenBao not configured. Set up the server address and login in Settings.".into(),
            )
        })?;
    let role_path = conn.bao_role_path.as_deref().ok_or_else(|| {
        CommandError::OpenBao("no role path configured for this connection".into())
    })?;
    let creds = bao.fetch_pg_creds(role_path).await?;
    let expiry =
        SystemTime::now().checked_add(Duration::from_secs(creds.lease_duration_secs));

    let ssl_mode =
        PgConnector::parse_ssl_mode(&conn.ssl_mode).map_err(|e| CommandError::Pg(e.0))?;
    let connector = PgConnector {
        host: conn.host.clone(),
        port: conn.port as u16,
        username: creds.username,
        password: creds.password,
        ssl_mode,
    };

    let budget = conn.slot_budget.max(1) as usize;
    let mut handle = ServerHandle::new(connector, budget);
    handle.credential_expiry = expiry;
    let mut state = handle.slot_manager.state();
    state.credential_expiry = expiry;
    registry.by_id.insert(id, handle);
    Ok(state)
}
```

Register `refresh_openbao_creds` in `lib.rs`.

#### D.2.2 `src/lib/tauri.ts` (modified)

Add to `SlotState`:

```typescript
export type SlotState = {
  budget: number;
  slots: SlotInfo[];
  credential_expiry: { secs_since_epoch: number; nanos_since_epoch: number } | null;
};
```

Add to `api`:

```typescript
refreshOpenBaoCreds: (id: number) =>
  invoke<SlotState>("refresh_openbao_creds", { id }),
```

#### D.2.3 `src/routes/+page.svelte` (modified)

**Add polling for expiry updates:**

```typescript
let expiryPollHandle = $state<ReturnType<typeof setInterval> | null>(null);

$effect(() => {
  const connected = Object.keys(connectedState).map(Number);
  if (connected.length === 0) {
    if (expiryPollHandle) { clearInterval(expiryPollHandle); expiryPollHandle = null; }
    return;
  }
  if (expiryPollHandle) return;
  expiryPollHandle = setInterval(async () => {
    for (const sid of connected) {
      try {
        const s = await api.getSlotState(sid);
        if (s) connectedState[sid] = s;
      } catch {}
    }
  }, 30_000);
});
```

**Add `refreshOpenBaoCreds`:**

```typescript
async function refreshOpenBaoCreds(id: number) {
  try {
    const state = await api.refreshOpenBaoCreds(id);
    connectedState[id] = state;
  } catch (err) {
    console.error("refresh failed", err);
  }
}
```

**Add "Refresh OpenBao credentials" to context menu** (`menuItemsFor`):

```typescript
if (t.kind === "server") {
  if (isConnected(t.conn.id)) {
    items.push({ action: "disconnect", label: "Disconnect" });
    if (t.conn.credential_source === "openbao") {
      items.push({ action: "refresh-openbao", label: "Refresh OpenBao credentials" });
    }
  } else {
    items.push({ action: "connect", label: "Connect..." });
    items.push({ action: "edit", label: "Edit connection..." });
  }
  // ... rest unchanged
}
```

**Add `"refresh-openbao"` to `menuAction`:**

```typescript
case "refresh-openbao":
  if (target.kind === "server") await refreshOpenBaoCreds(target.conn.id);
  break;
```

**Expiry display on server rows** (modify the server-row template around line 634):

```svelte
<div class="server-row">
  <Tree
    node={serverNode}
    {isConnected}
    {selectedDb}
    onSelectDb={selectDb}
    onContextMenu={openMenu}
    {onConnectServer}
  />
  <span class="slot-badge">
    {slotLabel(connectedState[serverNode.conn.id])}
    {#if connectedState[serverNode.conn.id]?.credential_expiry}
      {@const expiry = connectedState[serverNode.conn.id].credential_expiry}
      {@const expiryMs = expiry.secs_since_epoch * 1000 + expiry.nanos_since_epoch / 1_000_000}
      {@const remaining = expiryMs - Date.now()}
      {#if remaining < 60_000}
        <span class="expiry expiry-critical">expires in {Math.ceil(remaining / 1000)}s</span>
      {:else if remaining < 300_000}
        <span class="expiry expiry-warn">expires in {Math.ceil(remaining / 60_000)}m</span>
      {/if}
    {/if}
  </span>
</div>
```

**Add CSS** (in the `<style>` block):

```css
.expiry { font-size: 0.75rem; margin-left: 0.4rem; }
.expiry-warn { color: #b8860b; }
.expiry-critical { color: #b00020; font-weight: bold; }
```

### D.3 Implementation order

1. Add `refresh_openbao_creds` command + register in `lib.rs`
2. Add `credential_expiry` to `SlotState` + `refreshOpenBaoCreds` to `tauri.ts`
3. Add expiry polling + `refreshOpenBaoCreds` function to `+page.svelte`
4. Add "Refresh OpenBao credentials" to context menu
5. Add expiry display to server rows (template + CSS)
6. Run `./test.sh`

### D.4 Gotchas

- **Clock skew.** `SystemTime::now()` (Rust) and `Date.now()` (JS) differ by at most a few seconds. The thresholds (60s, 300s) provide generous buffer.
- **`setInterval` with `$effect`.** The effect runs when `connectedState` is assigned a new object (re-assigned, not mutated in place). Keys added to the record trigger the effect to re-evaluate. If `connectedState` is mutated in place without re-assigning the container, the effect won't re-trigger. In Quill, `connectedState[id] = ...` is a mutation on a `$state` record; Svelte 5 tracks this and re-runs the effect.
- **Disconnecting stops expiry polling.** When all servers are disconnected, `Object.keys(connectedState).length === 0`, the interval is cleared.
- **`refresh_openbao_creds` invalidates open tabs.** Disconnecting closes all slots; active tabs targeting the server will lose their DB handles. The frontend doesn't handle this automatically — the user must close/refresh tabs. This is acceptable for v1 since "Refresh" is a rare action.
- **Concurrent polling and refresh.** `refreshOpenBaoCreds` calls `api.refreshOpenBaoCreds(id)` which removes and re-inserts the server in the registry. The poll interval may fire during this window. The `getSlotState` call returns `null` if the server isn't in the registry, so no error.

### D.5 Test cases (manual)

1. Connect via OpenBao — observe `credential_expiry` is in the future.
2. Wait until <5 min remains — verify yellow badge appears.
3. Right-click -> "Refresh OpenBao credentials" — verify expiry is reset to the full TTL.
4. Disconnect server — verify expiry display disappears.
5. Connect via password source — verify no expiry display.
6. "Refresh OpenBao credentials" is not in context menu for non-OpenBao servers.
7. Polling: connect an OpenBao server, wait 30+ seconds, verify `connectedState` updates.

### D.6 Acceptance criteria

- `./test.sh` passes
- `cargo clippy -- -D warnings` passes
- Expiry countdown appears for OpenBao connections, disappears for password
- "Refresh OpenBao credentials" resets the expiry timer
- No stale intervals after disconnect

---

## Summary: files changed per session

| File | A | B | C | D |
|------|---|---|---|---|
| `src-tauri/Cargo.toml` | — | M | — | — |
| `src-tauri/migrations/0004_openbao.sql` | C | — | — | — |
| `src-tauri/src/store/mod.rs` | M | — | — | — |
| `src-tauri/src/commands/mod.rs` | M | M | — | M |
| `src-tauri/src/lib.rs` | M | M | — | M |
| `src-tauri/src/slots/mod.rs` | — | M | — | — |
| `src-tauri/src/registry.rs` | — | M | — | — |
| `src-tauri/src/openbao.rs` | — | C | — | — |
| `src/lib/tauri.ts` | M | — | M | M |
| `src/routes/+page.svelte` | M | — | M | M |
| `src/lib/Tree.svelte` | — | — | — | — |

Key: C = create, M = modify, — = untouched
