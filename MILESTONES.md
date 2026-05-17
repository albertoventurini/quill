# Quill — Milestones

Per-milestone briefs for the v1 plan in `PRD.md` §11. Each entry is the seed
context a spec writer needs to expand into a concrete task spec (see
`tasks/m1-*.md` for the style M1 was broken into).

---

## M1 — Shell & connect

**Goal:** A Tauri window opens, the user adds a saved Postgres server, connects to it, runs a hardcoded `SELECT 1`, and sees rows — all gated by a slot manager that refuses to exceed the per-server budget.

**Scope:**
- Tauri 2 + Svelte 5 + SvelteKit (`adapter-static`) scaffold; `./install.sh`, `./run.sh`, `./test.sh`, `./build.sh` wrappers.
- Local SQLite app store at `<app_data_dir>/quill.sqlite` with migration `0001_initial.sql` creating the `connections` table per `PRD.md` §10. Module: `src-tauri/src/store/mod.rs` exposing `open`, `list`, `get`, `insert`, `delete` and a `StoreError` enum.
- Slot manager: `src-tauri/src/slots/mod.rs` with `SlotManager<C: Connector>`, `SlotGuard`, `SlotState`, `SlotInfo`, `SlotError`. Enforces rules 1–4 from `PRD.md` §6 (rule 4 fails fast in M1 — no queueing). LRU eviction across idle slots. `set_budget` can only grow in M1.
- Postgres connector binding the slot manager to `sqlx::PgConnection` (single connection, not a pool — the slot manager *is* the pool). One `Connector` impl per saved server.
- Tauri command surface (`src-tauri/src/commands/...`): `list_connections`, `save_connection`, `delete_connection`, `connect_server`, `disconnect_server`, `run_query`, `get_slot_state`. Errors serialize as `{ kind, message }` (`CommandError`).
- Minimal Svelte UI: two-pane split (`src/routes/+page.svelte`) — left = saved connections list + "Add connection" modal; right = `<textarea>` SQL + "Run on database…" input + Run button + `<pre>` result. `src/lib/tauri.ts` is the typed bridge.
- Slot indicator `[used/budget]` next to each connection.

**Out of scope:** tree browsing, CodeMirror, result grid, autocomplete, cancellation, tabs, history, saved queries, CSV, OS keychain. Password is re-entered per Connect.

**Depends on:** Nothing. This is the foundation.

**Current state when this runs:** Empty Tauri 2 scaffold with default `greet` command. As of this writing M1 is mid-flight: M1.2 (store) and M1.3 (slot manager) are committed; M1.4 (PgConnector), M1.5 (Tauri commands), and M1.6 (UI shell) are specced in `tasks/` but not built.

**Key constraints:**
- **Principle 1 (no hidden connections):** Every `acquire` must come from a user action. No background tasks, timers, keepalives, or test-on-borrow anywhere.
- **Principle 2 (pool is a budget):** `SlotManager` must refuse to silently exceed `budget`; rule 4 returns `SlotError::AllBusy` rather than opening an extra connection.
- Use `sqlx` with `runtime-tokio` and the `postgres` feature. Do **not** use `PgPool` as the connection store — the slot manager owns each connection directly.
- Rust edition 2024; `cargo fmt` + `cargo clippy -- -D warnings`; Svelte 5 runes only (no legacy `$:`).
- The frontend never talks to Postgres directly — all DB I/O lives in `src-tauri/`.

**Seed context for spec writer:** The slot manager is the reason this project exists; getting its semantics right matters more than any UI work in M1. The internal lock should wrap a `Vec<Slot>` (each: `Option<C::Conn>`, `Option<String> database`, `Instant last_used`, `bool busy`); `tokio::sync::Mutex` is fine since `connect` is async. LRU eviction (rule 3) operates only over *idle* slots bound to a different database — never evict a busy slot to satisfy a different DB, since the guard still holds the connection. `Connector::connect` returning an error must restore the slot to `free`, not leave it half-bound. `disconnect_all` should close idle slots immediately and let busy ones close on guard drop — document the choice. On the Postgres side, capture the backend process ID and cancel secret returned at startup (sqlx exposes them) and stash them on the slot so M3 can build a `CancelRequest` later — even though cancellation itself isn't wired in M1. The frontend `CommandError` shape (`kind` + `message`) must mirror the Rust serde tagging exactly; mismatches surface as unreadable `[object Object]` rejections in the webview. Don't bother with CSS — M3/M6 own polish. For `Cargo.toml`, the dep set is already mostly there (`sqlx 0.8`, `tokio 1`, `thiserror 2`, `async-trait 0.1`, `serde 1`); add `sqlx` `postgres` feature when M1.4 lands.

---

## M2 — Tree

**Goal:** A connected server expands into a lazy tree (databases → schemas → tables / views / materialized views / functions), with each level fetched once on first expand and cached in the local SQLite store.

**Scope:**
- New backend module `src-tauri/src/introspect/mod.rs` with one-shot fetches:
  - `list_databases(server_id)` — `SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate`.
  - `list_schemas(server_id, db)` — `pg_namespace` minus `pg_*` / `information_schema`.
  - `list_relations(server_id, db, schema)` — `pg_class` filtered by `relkind IN ('r','v','m')` joined to `pg_namespace`; one call returns tables, views, matviews tagged by kind.
  - `list_functions(server_id, db, schema)` — `pg_proc` joined to `pg_namespace`.
- Migration `0002_schema_cache.sql` creating `schema_cache(server_id, database, payload_json, fetched_at)` per `PRD.md` §10. Cache key = `(server_id, database)`; value = a JSON blob holding the whole introspected tree for that DB.
- Cache strategy: **manual refresh only** (resolves open question §12 in favor of "no background work"). First expand misses → query Postgres (acquires a slot for the duration of introspection) → write cache. Subsequent expands hit cache. Right-click → Refresh re-runs introspection and overwrites the cache.
- Tauri commands: `list_databases`, `list_schemas`, `list_relations`, `list_functions`, `refresh_schema_cache(server_id, database)`.
- Frontend: replace the flat connection list with a tree component (Svelte 5, recursive). Lazy expansion; loading spinner per node; right-click context menu (connect / disconnect / refresh / copy name); per-node visual state (connected, connecting, disconnected, error).
- Slot indicator already shipped in M1 stays; expanding a DB will visibly bump it (`[1/2]` while the introspection query runs).

**Out of scope:** any use of the cache to power autocomplete (that is M4); editor changes; tree drag-drop or favorites.

**Depends on:** M1. Needs the slot manager (introspection acquires a slot for one query), the store (to add the new table via a fresh migration), and the Tauri command plumbing.

**Current state when this runs:** M1 is complete. `src-tauri/src/{store,slots,commands}` are populated; `src/routes/+page.svelte` has the two-pane shell with a flat connection list; CodeMirror is *not* yet present (still a textarea); the result is a `<pre>` blob.

**Key constraints:**
- **Principle 1:** No background refresh. Cache invalidation is strictly manual (resolves §12 in favor of the "no background work" principle).
- **Principle 3 (caching over re-fetching):** Don't issue an introspection query if the cache hit is valid. The only writes to `schema_cache` are first-expand and explicit refresh.
- Each introspection query acquires a slot via `SlotManager::acquire` like any other query — no shortcut path. This is what makes the `[1/2]` indicator honest.
- All four object kinds (tables, views, matviews, functions) come from system catalogs; do **not** parse `\d` output or shell out to `psql`.

**Seed context for spec writer:** The single biggest design decision is **one cache row per (server, database)** vs. one row per (server, database, schema). Pick the former — schemas inside a DB are cheap to fetch together, the cache payload is small (a few hundred KB even for huge schemas), and it makes Refresh atomic. The payload schema should be versioned (`{"v": 1, "schemas": [...]}`) so M4's autocomplete can rely on the shape. `relkind` is `'r'` table, `'v'` view, `'m'` matview, `'p'` partitioned table (treat as table); exclude `'i'` (index), `'S'` (sequence), `'t'` (TOAST), `'c'` (composite) from v1. For functions, `pg_proc.prokind` distinguishes `'f'` function, `'p'` procedure, `'a'` aggregate, `'w'` window — return them all but tag the kind so the tree icon can differ. Watch out for `search_path`: don't filter by it here; the tree shows raw catalog contents. Caching `search_path` itself is M4's job. The right-click "Refresh" should refresh the *deepest cached level* the user invoked it on, not always the whole DB — but the cache key is per-DB, so in practice Refresh on any node under a DB re-introspects that DB. Document this; users will ask. SvelteKit's `adapter-static` means the tree component must work without SSR — runes only; no `load` functions hitting the backend at navigation time.

---

## M3 — Editor & run

**Goal:** Replace the textarea + `<pre>` shell with a real SQL editor (CodeMirror 6, Postgres dialect) and a real result grid, with inline errors and working Cancel via Postgres's out-of-band `CancelRequest`.

**Scope:**
- CodeMirror 6 integration: `@codemirror/state`, `@codemirror/view`, `@codemirror/lang-sql` (configured for the PostgreSQL dialect), `@codemirror/commands` for undo/redo/find/comment-toggle. One editor instance per server (still no tabs — M5).
- `Cmd/Ctrl+Enter` runs the current statement or selection. Statement boundary detection: split on top-level `;` (cheap heuristic; defer multi-statement scripts to the §12 open question — for M3, run only the first statement if multiple are present, and warn).
- Backend `query` module: `run_query(server_id, db, sql) -> QueryResult { columns, rows, row_count, duration_ms }`. Streams rows from `sqlx::query` into a buffer; the slot guard is held for the full call.
- **Cancellation (load-bearing):** at connect time, capture the backend PID + cancel key returned by Postgres's startup message and stash them on the `Slot`. Build a `cancel_query(server_id)` Tauri command that opens a fresh TCP connection to `host:port`, sends a `CancelRequest` packet (length 16, code `80877102`, then PID + key as big-endian u32s), and closes — **without** touching the slot manager. This connection does not count against the budget. `sqlx` does not expose `CancelRequest` directly; implement the packet by hand against a raw `TcpStream` (or `TlsStream` if SSL is on).
- Result grid component (Svelte 5): sortable columns, resizable, click-to-expand cell preview for long values, read-only. Status line below shows `rows · duration_ms · slot`.
- Inline errors below the editor (not modal); `CommandError.message` rendered verbatim.
- Streaming + pagination: fetch in chunks; "Load more" button appends; no auto-fetch. Resolve §12 open question by picking a default chunk size (suggest 1000 rows) and never auto-flushing.

**Out of scope:** schema-aware autocomplete (M4); multiple tabs (M5); CSV export (M5); query history append (M5 — but log the *shape* of the data needed so M5 can wire it up cheaply).

**Depends on:** M1 (slot manager + commands), M2 (not strictly required, but the tree is what the user will be clicking around in while testing this).

**Current state when this runs:** M2 is complete. The UI has a working tree on the left and the ugly textarea/pre on the right. Backend has `store`, `slots`, `introspect`, plus M1's command set.

**Key constraints:**
- **Principle 4 (cancellation is first-class):** Cancel must work, must be visible while a query runs, and must **not** consume a slot.
- **Principle 5 (sync-feeling UI, async core):** While a query runs, browsing the (already-cached) tree must stay responsive. Run the query on a tokio task; the slot guard is held there, not on the UI thread.
- CodeMirror 6 is ESM and SSR-hostile; mount it inside an `onMount`/`$effect` so SvelteKit's static adapter doesn't try to pre-render it.
- Result grid is **read-only in v1** (PRD non-goal). Don't even add hooks for editing.

**Seed context for spec writer:** The PostgreSQL `CancelRequest` is the single most error-prone part of M3 — `sqlx` deliberately doesn't expose it, so you'll write the wire packet yourself. The relevant message is documented in the Postgres protocol docs as the `CancelRequest` message: 16 bytes, big-endian, opened on a fresh connection that gets closed immediately after the write. The PID + secret are returned in the `BackendKeyData` message during connection startup; intercept them via `PgConnectOptions::log_statements(...)` or — more reliably — by replacing the sqlx connect with a thin wrapper that captures them before handing the connection off to the slot. Bench-test this against a real `pg_sleep(10)` before declaring victory. CodeMirror 6 has a fiddly module layout — install exact peer deps (`@codemirror/lang-sql` pins specific `@codemirror/*` versions). The PostgreSQL dialect in `lang-sql` already handles keyword highlighting, identifier quoting, and dollar-quoted strings; don't reinvent. For the result grid, resist the urge to pull in `ag-grid` or `tabulator` — a hand-rolled `<table>` with CSS grid for resizing is ~150 lines and matches the "no heavy component framework" call in `PRD.md` §9. Cell preview for long values: `<dialog>` with the raw text; bypass JSON serialization for strings. Errors from Postgres include line/column info — surface it next to the inline error so users can find the offending token. The "run current statement vs selection" rule: if the editor has a selection, run that verbatim; otherwise run the statement containing the cursor (split on top-level `;`). Statement parsing in M3 stays naive — `sqlparser-rs` enters in M4.

---

## M4 — Autocomplete

**Goal:** The SQL editor offers schema-aware completions — schemas, tables/views/matviews, columns, keywords — fed entirely from the cached introspection. No extra DB queries on keystrokes.

**Scope:**
- Read `search_path` once per connection (on first acquire after Connect) via `SHOW search_path`; cache on the connection-level state. Drives unqualified-name suggestions.
- Extend `schema_cache` payload to include columns per relation (name, type, nullable, position). Bump payload schema version and write a one-time re-introspect on cache miss for v1-shaped payloads.
- Backend `parse` module (or inside `query`): `parse_from_clause(sql, cursor_offset) -> FromContext { tables: [(schema?, name, alias?)], cursor_token: TokenKind }` using `sqlparser-rs`. Exposed as a Tauri command — the editor calls it on completion-trigger, not per keystroke.
- CodeMirror completion source (frontend) that:
  - Suggests **schemas** in `FROM`/`JOIN` position and as the first segment of qualified names.
  - Suggests **tables/views/matviews** after a schema qualifier and unqualified for objects reachable via `search_path`.
  - Suggests **columns** after a table or alias qualifier; unqualified columns drawn from tables in scope per the parsed `FROM` clause.
  - Suggests **keywords** always.
- Case-insensitive matching; auto-quote identifiers that require it (mixed case, reserved words, contain non-`[a-z0-9_]`).
- Completion data flow: the editor caches the per-`(server, database)` schema payload in a Svelte store on first request, then computes completions locally. No round-trip per keystroke.

**Out of scope:** function-argument completion; CTE alias resolution (`WITH foo AS (...) SELECT foo.<TAB>`) — explicitly defer to v1.1; smart join inference.

**Depends on:** M3 (CodeMirror in place) and M2 (cache populated). Crucially needs the cache payload to grow a `columns` array — plan the schema bump as part of M4.

**Current state when this runs:** M3 is complete. CodeMirror is live, the result grid works, Cancel works. The schema cache holds tables/views/matviews/functions but no column metadata yet. `sqlparser-rs` is not yet a dep.

**Key constraints:**
- **Principle 1:** Autocomplete must **not** open Postgres connections. All data comes from the cache. If the cache is cold for a `(server, db)`, the completion source returns keywords + a hint to expand the DB in the tree first.
- **Principle 3:** Don't background-refresh the cache to keep autocomplete current — same manual-refresh rule as M2.
- `sqlparser-rs` is Rust-only; do alias resolution in a Tauri command, not in JS. (PRD §7.3 spells this out.)
- Case-insensitive matching honors Postgres folding rules — unquoted identifiers fold to lowercase; quoted ones are case-sensitive. The auto-quoter must round-trip correctly.

**Seed context for spec writer:** The single biggest hidden cost is **column metadata in the cache**. M2 deliberately didn't fetch columns; M4 needs them per relation. The right move is to bump `schema_cache.payload_json` to `{"v": 2, ...}` and have introspection now also pull `pg_attribute` (`attname`, `atttypid::regtype::text`, `attnotnull`, `attnum`) for every relation in one query joined to `pg_class`. Don't fetch columns lazily per-relation — one big query is faster and matches the "cache the whole DB at once" decision. Migration-wise, no SQLite schema change is needed; just invalidate v1 payloads on read. `sqlparser-rs` (Postgres dialect) handles aliases correctly but its AST is verbose; you only need the `FROM` clause walker — extract `(schema?, name, alias?)` tuples and ignore the rest. The cursor-position-to-context mapping is the tricky part: tokenize the buffer up to the cursor, look at the last 1–2 tokens to decide context (`FROM` → suggest schemas/tables, `.` after an identifier → suggest tables-in-schema or columns-in-table, etc.). CodeMirror's `autocompletion` extension calls your source with a `CompletionContext`; return `{from, options}` where `from` is the start of the partial token. The auto-quoter: an identifier needs `"..."` if it contains any character outside `[a-z0-9_]` or matches a reserved word — `sqlparser-rs` has a reserved-word list you can lean on. `search_path` parsing: it's a comma-separated string; `"$user"` resolves to the connection user; `public` is the default tail. Keep all this in Rust and ship the resolved schema list to the frontend as a plain array.

---

## M5 — Tabs, history, saved, CSV

**Goal:** The editor supports multiple tabs (each pinned to a `(server, database)`), every executed query is logged, users can save and recall named snippets, and the result grid exports to CSV.

**Scope:**
- Frontend: tab bar above the editor. Each tab holds `{ id, server_id, database, sql, result?, dirty }`. Add tab `+`, close `×`, switch by click. Switching databases for an existing tab is an explicit action (right-click → "Change database…"), not a passive dropdown.
- Migration `0003_history_saved.sql`: `query_history(id, ts, server_id, database, sql, duration_ms, row_count, ok, error)` and `saved_queries(id, name, scope, server_id, sql, created_at)` per `PRD.md` §10. `scope` is `'global'` or `'server'`.
- Backend `history` module: `append(record)` called from `run_query` on every result (success or error); `list(limit, filter?)`; trim-on-insert to retention limit.
- Backend `saved` module: `list(server_id?)`, `create`, `delete`, `rename`.
- Tauri commands: `list_history`, `clear_history`, `list_saved`, `save_query`, `delete_saved`, `rename_saved`.
- Side panel (Svelte): "History" and "Saved" tabs. History entry double-click → opens a new tab pre-filled with the SQL pinned to the same `(server, database)`. Saved entry double-click → opens a new tab pre-filled.
- CSV export: right-click on the result grid → "Export CSV"; serializes the *currently materialized* buffer (whatever "Load more" has fetched so far) to a file via Tauri's `dialog` plugin. RFC 4180 escaping.
- Settings: history retention setting (default 1000) editable via the future M6 settings panel — for M5, the value lives in a constant.

**Out of scope:** per-tab queue UX (PRD §12 open question); tag/folder organization for saved queries; importing CSV; sharing.

**Depends on:** M3 (the `query` path is where history append hooks in; the grid is where CSV export hooks in). Independent of M4 — autocomplete works the same across tabs.

**Current state when this runs:** M4 is complete. CodeMirror with schema-aware completion is in place; the result grid is mature; cancellation works. There is exactly one editor surface and one result surface in the app.

**Key constraints:**
- **Principle 1:** Saving a query, browsing history, listing saved entries — all of these are local-SQLite operations. They must not open Postgres connections. Re-running a history entry does, but only on explicit user trigger.
- Tabs are pinned to `(server, database)`. Don't let a tab silently drift to a different DB just because the user clicked elsewhere in the tree. The slot will be acquired against the *tab's* database, not the tree-selection's.
- Retention trim runs synchronously inside `history::append` (cheap, single delete by id range). No background job.

**Seed context for spec writer:** The "pinned to `(server, database)`" rule for tabs is the subtle bit — users will expect changing the tree selection to change the active tab's target DB, and it must not. Make this visible: each tab's title bar shows `server / database`, and the DB name is muted when it matches the tree selection, highlighted when it doesn't. History retention is a per-row trim, not a `VACUUM` — `DELETE FROM query_history WHERE id IN (SELECT id FROM query_history ORDER BY id DESC LIMIT -1 OFFSET ?)` keeps the newest N. Failed queries go into history too (`ok=0`, `error` populated), so the history panel can show red rows the user can re-edit. Saved-query scope: `'global'` rows have `server_id NULL`; `'server'` rows are filtered to the currently-selected server. The right-click on the result grid: Tauri's webview blocks the native context menu by default; you'll need to either re-enable it or build a small custom menu component. CSV escaping is RFC 4180 — wrap in `"..."` if a field contains `,`, `"`, or newline; double interior `"`. Streams that have only partially loaded ("Load more" not exhausted) export only what's in the buffer; print a visible note in the dialog or use the file name (`results-partial-12345.csv`) so users aren't surprised. Tab IDs should be stable across reloads only if you bother to persist tabs — for M5, in-memory is fine; persistence can wait.

---

## M6 — Polish

**Goal:** Quill is something the user wants to run daily: passwords live in the OS keychain, slot indicators are informative, settings are editable in-app, and `./build.sh` produces a packaged binary the user can install.

**Scope:**
- **Keyring:** add the `keyring` crate. On `save_connection`, write the password to the OS keychain under a deterministic key (`com.alberto.quill:<connection-id>`) and store the key as `connections.password_ref`. `connect_server` reads from the keychain. The "prompt-on-connect" alternative remains supported (`password_ref` NULL → ask).
- **Slot indicator polish:** hover over `[2/2]` reveals a tooltip listing each slot's currently-bound database and idle/busy state. Right-click on a server node → "Disconnect all" (calls `SlotManager::disconnect_all`) and "Edit slot budget" (calls `set_budget`, still grow-only or — if implemented now — a graceful shrink that waits for in-flight guards).
- **Settings panel:** a dedicated route/dialog with: default SSL mode, history retention, schema-cache refresh hint (no TTL — just a manual reminder), theme (light/dark), font size for the editor.
- **Packaging:** verify `./build.sh` produces `.deb` and `.AppImage`; populate Tauri bundle metadata (identifier, version, icons, description); explicitly disable the Tauri updater (or wire it — open question for v1).
- **Visual polish across the app:** consistent spacing, button styles, focus states, dark mode. Replace any remaining textarea-era leftovers.
- Resolve `PRD.md` §12 open questions still outstanding with concrete v1 calls (format-on-save, multi-statement scripts, pin-slot action) or document them as v1.1 deferrals.

**Out of scope:** anything in `PRD.md` §3 non-goals (other engines, ER diagrams, AI SQL, editable grid, plan visualization, multi-user). Auto-update infrastructure if not chosen.

**Depends on:** M5. Most of M6 is layered on a fully working app; the keyring change touches M1's `connect_server` flow.

**Current state when this runs:** M5 is complete. All feature surfaces exist; passwords are still re-entered per Connect; slot indicators show numbers but no detail; there's no settings UI; the app is functionally complete but visually rough.

**Key constraints:**
- The `keyring` crate uses platform backends (Secret Service on Linux, Keychain on macOS, Credential Manager on Windows). On Linux it requires a running keyring daemon — handle the "no backend" error path with a clear message ("OS keychain unavailable; falling back to prompt-on-connect").
- **Principle 1 still applies:** the keyring change doesn't open Postgres connections; it just changes where the password comes from.
- Packaging signing: not in scope unless the user wants to sign artifacts (they don't, per the "personal project" framing in `CLAUDE.md`). Unsigned `.deb` / `.AppImage` is fine.
- Don't introduce abstraction layers that hint at multi-engine support (`PRD.md` non-goals).

**Seed context for spec writer:** The keyring migration is the load-bearing change. Strategy: add a `password_ref` value of the form `keyring:<uuid>` on save; on read, look it up by that string. Pre-existing rows have `password_ref = NULL` and continue to prompt — no data migration needed. On Linux, GNOME Keyring / KWallet / KeePassXC all speak the Secret Service API; the `keyring` crate handles dispatch. Test on a headless setup: there's no daemon and the crate returns a specific error you can detect and fall back from. "Edit slot budget" reopens the `set_budget` question: M1 said grow-only; if M6 wants to support shrinking, it must drain idle slots first and refuse to shrink below the count of busy slots (return an error and surface it in the UI rather than killing in-flight queries). The settings panel is the cleanest place to surface the connection-budget defaults too, so users can change the default for *new* connections without editing JSON. Theme switching: CSS custom properties on `<html>`, toggled by a single class — don't pull in a theme library. Tauri bundle identifiers must be set in `tauri.conf.json` before `./build.sh` produces installable artifacts (the scaffold's default `com.tauri.dev` will not do); pick `com.alberto.quill` consistent with the app-data dir. Dark mode for CodeMirror needs the `@codemirror/theme-one-dark` package or a hand-rolled theme — pick one and stick with it. Open questions to close in M6: format-on-save (pick `pg_format` external OR pure-Rust `sqlparser` round-trip, document the trade-off in the settings panel); multi-statement script execution (v1: run sequentially, show last result, with a status line listing all run); pin-slot action (defer to v1.1 — surface it as a known limitation in the slot tooltip instead).
