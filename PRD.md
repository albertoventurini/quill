# Quill — PRD (v1)

## 1. Overview
Quill is a personal-use, desktop SQL client for PostgreSQL, built in Rust + Tauri with a Svelte frontend. It exists because mainstream clients (DBeaver, etc.) silently open many background connections, which trip the active-connection limits enforced by the user's organization. Quill's defining behavior is **strict, user-visible control over the number of active connections per server**.

## 2. Goals
- Browse Postgres servers (databases → schemas → tables) in a left-hand tree.
- Write SQL in a syntax-highlighted editor with schema-aware autocomplete.
- Run queries and view results as a table (or raw text), with cancellation.
- Stay within a small, configurable connection budget per server (default **2**).
- Feel responsive: UI never blocks on DB work; long queries don't freeze browsing other (already-loaded) parts of the tree.

## 3. Non-goals (v1)
- Other DB engines (MySQL, SQLite, MSSQL, etc.).
- ER diagrams, visual query builders, AI-generated SQL.
- Editing data through the result grid (read-only results in v1; you write `UPDATE`s yourself).
- Plan visualization beyond raw `EXPLAIN` output.
- Multi-user, sync, sharing, plugins.

## 4. Design principles (load-bearing)
1. **No hidden connections.** Every active connection is the result of an explicit user action: connect, expand a database, run a query, refresh a schema. No keepalives, no autocomplete fetches, no "test on borrow."
2. **Pool is a budget, not a default.** Each saved server has a slot count (default 2, user-set 1–N). The UI shows current usage. The app refuses to silently exceed it.
3. **Caching over re-fetching.** Schema introspection happens once per database, on first expand. Refresh is a manual action.
4. **Cancellation is first-class.** Every running query has a visible cancel affordance. Cancel uses Postgres's out-of-band `CancelRequest` — it does not consume a slot.
5. **Synchronous-feeling UI, async core.** Frontend talks to Rust over Tauri commands; Rust owns all state. The UI shows clear "busy" states rather than spinning while the DB blocks.

## 5. Architecture

**Process model:**
- Single Tauri process. Svelte frontend in the webview. Rust backend owns all DB state.
- Frontend → Rust via `invoke` (typed Tauri commands). Rust → Frontend via events (query progress, slot changes, errors).

**Backend modules:**
- `connections` — saved server configs (CRUD), password retrieval from OS keychain.
- `slots` — the slot manager (see §6).
- `introspect` — one-shot schema fetches; results cached in memory + persisted.
- `query` — run, stream rows, cancel.
- `history` — append-only query log.
- `store` — local SQLite for connections (metadata only), history, saved queries, schema cache.

## 6. Connection slot model (the heart of it)

A **slot** is a live `PgConnection` bound to **one database at a time**. Each saved server has *N* slots (default 2).

**Slot acquisition rules** (when something needs to talk to database X on server S):
1. A slot on S already bound to X and idle → reuse it.
2. A free (unbound) slot on S exists → bind it to X (connect).
3. An idle slot on S is bound to some other database Y → evict Y (close), rebind to X. LRU.
4. All slots on S are busy → the action queues behind the slot it's targeting, or fails fast with a clear message ("no free connection; cancel a running query or raise the slot budget").

**Visibility:** A small `[2/2]` indicator next to each server node shows used/budget. Hover reveals which databases the slots are currently bound to. Right-click on a server → "Disconnect all" / "Edit slot budget."

**Cancellation:** Uses the Postgres `CancelRequest` wire-level mechanism — a one-shot TCP connection that does **not** count against the slot budget.

## 7. Features (v1)

### 7.1 Connection management
- Add/edit/delete saved server connections.
- Fields: name, host, port, default database (for first connect), user, password storage choice (OS keychain via `keyring` crate, or prompt-on-connect), SSL mode, slot budget (default 2).
- Passwords default to OS keychain.

### 7.2 Connection tree
- Servers → databases → schemas → tables / views / materialized views / functions.
- Lazy expansion. Each level fetched only when expanded. Results cached per (server, database).
- Right-click context menu: connect, disconnect, refresh, copy name.
- Visual states: connected, connecting, disconnected, error.

### 7.3 SQL editor
- **CodeMirror 6** with the SQL language pack, Postgres dialect.
- Schema-aware autocomplete fed from cached introspection:
  - **Schemas** in `FROM`/`JOIN` position and as the first segment of qualified names.
  - **Tables / views / matviews** after a schema qualifier, and unqualified for objects reachable via `search_path`.
  - **Columns** after a table or alias qualifier, and unqualified when tables are in scope from the current `FROM` clause.
  - **Keywords** always available.
- Alias resolution via `sqlparser-rs` parse of the `FROM` clause.
- `search_path` read once per connection and cached; drives unqualified suggestions.
- Case-insensitive matching; auto-quote identifiers that require it.
- Multiple tabs per server. Each tab is pinned to a `(server, database)` — switching databases for a tab requires explicit user action.
- Standard editing affordances: undo/redo, find, comment toggle, format (open question in §12).

### 7.4 Query execution
- `Cmd/Ctrl+Enter` runs the current statement (or selection).
- Status line: rows returned, time, slot used.
- **Cancel button** active while running.
- Errors shown inline below the editor (not modal).

### 7.5 Results panel
- Toggle: **Table** view (default) / **Text** view (raw).
- Table: sortable columns, resizable, cell preview on click for long values. Read-only.
- Pagination: streamed, with "Load more" (no auto-fetch).
- CSV export of current results (right-click → Export CSV).

### 7.6 Query history
- Every executed query appended to local SQLite with `(timestamp, server, database, sql, duration_ms, row_count, success)`.
- Browseable in a side panel; double-click to re-load into a new tab.
- Configurable retention (default: keep last 1000).

### 7.7 Saved queries / snippets
- User names and saves a query; appears in a "Saved" section in the side panel.
- Scoped per-server or global (user picks at save time).

## 8. UX / layout

```
┌──────────────────────────────────────────────────────────┐
│ Menu bar                                                 │
├────────────────┬─────────────────────────────────────────┤
│ Connections    │ tab1 │ tab2 │ + │                        │
│  └ prod [2/2]  ├─────────────────────────────────────────┤
│    ├ analytics │                                          │
│    │  └ public │   SQL editor (CodeMirror)                │
│    │     ├ ... │                                          │
│    └ postgres  ├─────────────────────────────────────────┤
│  └ local [0/2] │ Results ▸ [Table | Text]   Run ▸ Cancel ▸│
│                ├─────────────────────────────────────────┤
│ History        │                                          │
│ Saved          │   Result grid                            │
└────────────────┴─────────────────────────────────────────┘
```

## 9. Tech stack
- **Tauri 2.x**, **Rust** edition 2024.
- DB: `sqlx` (Postgres feature, `runtime-tokio`), used at `max_connections = budget`, `min_connections = 0`.
- SQL parsing (for autocomplete cursor context): `sqlparser-rs`.
- Local persistence: `rusqlite` or `sqlx-sqlite` for the app's own store.
- Secrets: `keyring` crate.
- Frontend: **Svelte 5** (runes), **CodeMirror 6** (`@codemirror/lang-sql`), vanilla CSS or a small utility lib — no heavy component framework.

## 10. Persisted data (local SQLite, separate from any user DB)
- `connections(id, name, host, port, default_db, user, ssl_mode, slot_budget, password_ref)`
- `schema_cache(server_id, database, payload_json, fetched_at)`
- `query_history(id, ts, server_id, database, sql, duration_ms, row_count, ok, error)`
- `saved_queries(id, name, scope, server_id, sql, created_at)`

## 11. Milestones
1. **M1 — Shell & connect:** Tauri app, Svelte UI shell, add/connect to one Postgres, run a hardcoded `SELECT 1`. Slot manager with N=2.
2. **M2 — Tree:** Lazy server → DB → schema → table browsing with cache.
3. **M3 — Editor & run:** CodeMirror 6, run query, show result table, errors, cancel.
4. **M4 — Autocomplete:** Schema-aware completion from cache.
5. **M5 — Tabs, history, saved, CSV.**
6. **M6 — Polish:** keyring, slot indicators, settings, packaging.

## 12. Open questions
- Format-on-save / format-shortcut: use `pg_format` (external) or pure-Rust `sqlparser` round-trip? Latter is uglier but no external dep.
- Result streaming threshold: fetch all rows up to N (e.g. 10k), then paginate? Or always paginate?
- What happens when the user runs a multi-statement script? v1: run sequentially, show last result; or refuse?
- Schema cache invalidation: TTL, or strictly manual? (Manual fits the "no background work" rule.)
- "Pin slot" action to prevent eviction of a slot bound to a frequently-used database — worth adding to v1, or defer?
- Multi-tab queueing UX: show queue position, or generic "waiting for slot"?
