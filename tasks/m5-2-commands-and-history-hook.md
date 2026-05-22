# M5.2 — Tauri commands for history/saved + history hook in `run_query`

## Goal

**Before (post-M5.1):** `history::append/list/clear` and `saved::list/create/delete/rename`
exist as Rust functions on the `SqlitePool`, but no Tauri command exposes
them and no part of the query path calls `history::append`. The frontend has
no way to read or write either table; every executed query is forgotten.

**After:** Six new Tauri commands form the bridge:

```
list_history(limit?, server_id?) -> Vec<HistoryRecord>
clear_history()                  -> ()
list_saved(server_id?)           -> Vec<SavedQuery>
save_query(NewSavedQuery)        -> SavedQuery
delete_saved(id)                 -> ()
rename_saved(id, new_name)       -> SavedQuery
```

`commands::run_query` is instrumented: on **both** the success and the error
path, it calls `history::append` exactly once with the time-to-first-chunk
and the success/failure shape. The append runs *outside* the slot critical
path (after the slot has been released back via the `OpenResult` insert or
the early-return on error) and any SQLite error from `append` is logged via
`eprintln!` and swallowed — a history hiccup must not turn a successful
Postgres query into a frontend error.

The TS bindings in `src/lib/tauri.ts` mirror the new types and add the six
`api.*` methods. No UI changes yet — those land in M5.4 (the side panel).
`pnpm check` and `./test.sh` are the M5.2 acceptance signals.

## Current state

### `src-tauri/src/commands/mod.rs` — the file this task edits

Read it in full first. Key points the M5.2 edits depend on:

- `CommandError` is the `#[serde(tag = "kind", content = "message")]`
  enum with variants `UnknownConnection`, `NotConnected`, `Slot`, `Pg`,
  `Store`, `Introspect`. M5.2 reuses `Store` for `HistoryError` and adds a
  new `Saved` variant.
- `run_query` (around line 296) is the hook site:
  ```rust
  query::run_query(server_id, &database, &sql, chunk, slot_manager, &results)
      .await
      .map_err(map_query_err)
  ```
  The append goes after this — once on the success branch, once on the
  error branch — without changing `query::run_query`'s signature. The
  Tauri layer is the right place: `history::append` needs the
  `SqlitePool`, which the query module has no business knowing about.
- `pool: State<'_, sqlx::SqlitePool>` is the existing pattern for
  injecting the store handle into a command. `run_query` doesn't take it
  today; M5.2 adds it.
- `From<store::StoreError>` and `From<introspect::IntrospectError>` are
  already implemented for `CommandError`. M5.2 adds two more.

### `src-tauri/src/history/mod.rs` — provider

`append`, `list`, `clear` and the `HistoryRecord` / `NewHistoryRecord` /
`HistoryFilter` types are M5.1's deliverables. Remove the
`#![allow(dead_code)]` at the top of the module — every public function
now has a caller.

### `src-tauri/src/saved/mod.rs` — provider

Same: M5.1 shipped `list`, `create`, `delete`, `rename` and the types.
Remove `#![allow(dead_code)]`. The TS shape is decided here in M5.2 —
specifically whether `SavedQuery.scope_str` becomes `scope` on the JSON
side. See "Design choices" below.

### `src-tauri/src/lib.rs` — `invoke_handler!` list

M5.2 adds six entries:

```rust
commands::list_history,
commands::clear_history,
commands::list_saved,
commands::save_query,
commands::delete_saved,
commands::rename_saved,
```

### `src/lib/tauri.ts` — TS mirror

Adds three new types (`HistoryRecord`, `SavedQuery`, `NewSavedQuery`,
optional `SavedScope` union) and six new `api.*` methods. No file structure
changes; the additions slot into the existing organisation.

## Design choices baked into this spec

- **`history::append` runs in `commands::run_query`, not `query::run_query`.**
  The query module is pure execution; it has no business taking a
  `SqlitePool`. The commands layer is already the place where Tauri state
  (registries, pools) meets the pure modules.
- **Single append per query.** Decided in the M5 spec discussion: one
  INSERT at the terminal of `run_query`. `duration_ms` records the
  time-to-first-chunk (`run_query`'s elapsed time as measured by
  `query::run_query`'s `Instant::now()`-based logic, surfaced via
  `RunResult.duration_ms_so_far`). Errors during `fetch_more` do **not**
  add a second row — the query itself succeeded; the cursor died later.
- **Append failures are logged and swallowed.** `eprintln!` to the Tauri
  log, return the original `RunResult` / `CommandError` to the frontend.
  This is the principled choice: history is observability, not a
  contract. Surfacing SQLite errors in the query response would conflate
  two failure modes the user can't act on independently.
- **`SavedScope` is a TypeScript union, not a Rust enum on the wire.**
  The Rust `SavedScope` enum already serializes lowercase
  (`#[serde(rename_all = "lowercase")]`), so the JSON shape is the string
  `"global"` / `"server"`. Add a `#[serde(rename = "scope")]` on
  `SavedQuery.scope_str` so the JSON key is `scope` (not `scope_str`); the
  TS mirror becomes `scope: "global" | "server"`. This is the M5.1 gotcha
  resolved in M5.2.
- **`list_history` defaults `limit` to `HISTORY_RETENTION` on the
  backend.** The frontend can pass any limit; missing argument means
  "return everything we keep."
- **`save_query` returns the full row.** Same idiom as `save_connection`
  — the frontend uses the returned `id` to reference the row in subsequent
  `delete_saved` / `rename_saved` calls without a refetch.
- **No `CommandError::History` variant.** `HistoryError` only contains
  `Sqlx`; map it to `CommandError::Store`, same as `store::StoreError`.
  Both are "local SQLite went wrong" — keep the user-facing kind set
  small.
- **New `CommandError::Saved(String)` variant.** `SavedError` has three
  shapes (`Sqlx`, `NotFound`, `DuplicateName`); the latter two are
  user-actionable ("a query named X already exists" needs different UI
  treatment than "DB error"). A dedicated `Saved` kind lets the M5.4 UI
  branch on it for the rename/save flows.
- **Type-only TS import for `HistoryRecord` etc.** Same convention as
  `SchemaPayload` — keeps the runtime bundle lean.

## Deliverables

### 1. `src-tauri/src/commands/mod.rs` — six new commands + run_query hook

**Add to `CommandError`:**

```rust
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum CommandError {
    UnknownConnection(String),
    NotConnected(String),
    Slot(String),
    Pg(String),
    Store(String),
    Introspect(String),
    Saved(String),   // <-- new
}
```

Add the matching arm to `Display`:

```rust
| Self::Saved(msg) => write!(f, "{msg}"),
```

**Add error converters near the existing `From` impls:**

```rust
impl From<crate::history::HistoryError> for CommandError {
    fn from(e: crate::history::HistoryError) -> Self {
        Self::Store(e.to_string())
    }
}

impl From<crate::saved::SavedError> for CommandError {
    fn from(e: crate::saved::SavedError) -> Self {
        Self::Saved(e.to_string())
    }
}
```

**Add the six commands.** Place them after the existing
`refresh_schema_cache` command and before the cancellation section, to keep
the file organised by feature group:

```rust
// ═══════════════════════════════════════════════════════════════════════════
// History
// ═══════════════════════════════════════════════════════════════════════════

use crate::history::{self, HistoryFilter, HistoryRecord, NewHistoryRecord};

#[tauri::command]
pub async fn list_history(
    limit: Option<i64>,
    server_id: Option<i64>,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<Vec<HistoryRecord>, CommandError> {
    let lim = limit.unwrap_or(history::HISTORY_RETENTION as i64);
    Ok(history::list(&pool, lim, HistoryFilter { server_id }).await?)
}

#[tauri::command]
pub async fn clear_history(
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<(), CommandError> {
    history::clear(&pool).await?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Saved queries
// ═══════════════════════════════════════════════════════════════════════════

use crate::saved::{self, NewSavedQuery, SavedQuery};

#[tauri::command]
pub async fn list_saved(
    server_id: Option<i64>,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<Vec<SavedQuery>, CommandError> {
    Ok(saved::list(&pool, server_id).await?)
}

#[tauri::command]
pub async fn save_query(
    new: NewSavedQuery,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<SavedQuery, CommandError> {
    Ok(saved::create(&pool, new).await?)
}

#[tauri::command]
pub async fn delete_saved(
    id: i64,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<(), CommandError> {
    saved::delete(&pool, id).await?;
    Ok(())
}

#[tauri::command]
pub async fn rename_saved(
    id: i64,
    new_name: String,
    pool: State<'_, sqlx::SqlitePool>,
) -> Result<SavedQuery, CommandError> {
    Ok(saved::rename(&pool, id, &new_name).await?)
}
```

**Hook `history::append` into `run_query`.** Rewrite the existing
`run_query` body. The shape is: take `pool` as new state, run the query,
build the history record from the outcome, append (log-and-swallow), then
return.

```rust
/// Run a SQL query against a connected server.  Opens a server-side cursor
/// and returns the first chunk (default 1000 rows).  Subsequent chunks are
/// fetched via [`fetch_more`]; the cursor is closed with [`close_result`].
///
/// Every call appends one row to `query_history` — on success with the
/// time-to-first-chunk and the row's `ok=true`; on failure with the time
/// elapsed before the error and `ok=false`.  History failures are logged
/// and swallowed — they never alter the response visible to the user.
#[tauri::command]
pub async fn run_query(
    server_id: i64,
    database: String,
    sql: String,
    chunk_size: Option<usize>,
    pool: State<'_, sqlx::SqlitePool>,
    registry: State<'_, ServerRegistry>,
    results: State<'_, ResultRegistry>,
) -> Result<RunResult, CommandError> {
    let handle = registry
        .by_id
        .get(&server_id)
        .ok_or_else(|| CommandError::not_connected(server_id))?;
    let slot_manager = handle.slot_manager.clone();
    drop(handle);

    let chunk = chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE);

    // Measure wall-clock as a fallback for the error path — on success we
    // prefer the more accurate `duration_ms_so_far` returned by the query
    // module (which excludes our pre-flight setup).
    let start = std::time::Instant::now();

    let outcome = query::run_query(server_id, &database, &sql, chunk, slot_manager, &results).await;

    let (record, response) = match outcome {
        Ok(run) => {
            let record = NewHistoryRecord {
                server_id,
                database: database.clone(),
                sql: sql.clone(),
                duration_ms: run.duration_ms_so_far as i64,
                ok: true,
                error: None,
            };
            (record, Ok(run))
        }
        Err(e) => {
            let err = map_query_err(e);
            let record = NewHistoryRecord {
                server_id,
                database: database.clone(),
                sql: sql.clone(),
                duration_ms: start.elapsed().as_millis() as i64,
                ok: false,
                error: Some(err.to_string()),
            };
            (record, Err(err))
        }
    };

    // Best-effort: log SQLite errors but never propagate.
    if let Err(history_err) = history::append(&pool, record).await {
        eprintln!("history::append failed: {history_err}");
    }

    response
}
```

Key details:

- The `pool: State<'_, sqlx::SqlitePool>` parameter is **new** on this
  command. Tauri's `invoke_handler` picks it up from `app.manage(pool)`
  which is already wired in `src-tauri/src/lib.rs`. No change to `lib.rs`
  is needed *for the pool plumbing*; only the new commands need to be
  registered (see deliverable #3).
- `record.duration_ms` for the success path comes from `run.duration_ms_so_far`
  (set by `query::run_query` as the time from `BEGIN` to first FETCH
  return); for the error path it comes from a local `Instant` (the query
  module's instant isn't surfaced through `QueryError`).
- The `CommandError::Display` impl exists on the type; `err.to_string()`
  produces the user-facing message. That's the same string the frontend
  sees in the response, so history rows and the inline error stay
  consistent.
- Cloning `sql` and `database` is cheap and necessary — we move them into
  the record before the `?` would consume them. (`sql` is an owned
  `String` parameter; cloning preserves the `Ok(run)` branch's structure.)

### 2. `src-tauri/src/history/mod.rs` — remove dead-code allow

```rust
// Delete this line at the top of the file:
#![allow(dead_code)]
```

Every `pub` function now has a caller (`commands::list_history`,
`commands::clear_history`, `commands::run_query`).

### 3. `src-tauri/src/saved/mod.rs` — remove dead-code allow and add JSON-key rename

```rust
// Delete:
#![allow(dead_code)]

// On SavedQuery, change the scope field annotation from:
#[sqlx(rename = "scope")]
pub scope_str: String,

// to (add a serde rename so the JSON key is `scope`, not `scope_str`):
#[sqlx(rename = "scope")]
#[serde(rename = "scope")]
pub scope_str: String,
```

The `scope()` accessor on `SavedQuery` stays — it's still useful Rust-side
for branching on `SavedScope::Global` vs `Server` in any future internal
code.

### 4. `src-tauri/src/lib.rs` — register six commands

Insert after `commands::analyze_completion,` in the `generate_handler!`
list:

```rust
commands::list_history,
commands::clear_history,
commands::list_saved,
commands::save_query,
commands::delete_saved,
commands::rename_saved,
```

No other `lib.rs` change needed.

### 5. `src/lib/tauri.ts` — TS mirror + six API methods

Insert near the existing types (e.g. after `CancelOutcome`):

```ts
// ── History (mirrors history::HistoryRecord) ──

export type HistoryRecord = {
  id: number;
  ts: string;
  server_id: number;
  database: string;
  sql: string;
  duration_ms: number;
  ok: boolean;
  error: string | null;
};

// ── Saved queries (mirrors saved::SavedQuery / NewSavedQuery) ──

export type SavedScope = "global" | "server";

export type SavedQuery = {
  id: number;
  name: string;
  scope: SavedScope;
  server_id: number | null;
  sql: string;
  created_at: string;
};

export type NewSavedQuery = {
  name: string;
  scope: SavedScope;
  server_id: number | null;
  sql: string;
};
```

Extend the `CommandError.kind` union with `"Saved"`:

```ts
export type CommandError = {
  kind:
    | "UnknownConnection"
    | "NotConnected"
    | "Slot"
    | "Pg"
    | "Store"
    | "Introspect"
    | "Saved";          // <-- new
  message: string;
};
```

Add to the `api` object (at the end, before the closing brace):

```ts
listHistory: (limit: number | null = null, serverId: number | null = null) =>
  invoke<HistoryRecord[]>("list_history", { limit, serverId }),

clearHistory: () =>
  invoke<void>("clear_history"),

listSaved: (serverId: number | null = null) =>
  invoke<SavedQuery[]>("list_saved", { serverId }),

saveQuery: (newQuery: NewSavedQuery) =>
  invoke<SavedQuery>("save_query", { new: newQuery }),

deleteSaved: (id: number) =>
  invoke<void>("delete_saved", { id }),

renameSaved: (id: number, newName: string) =>
  invoke<SavedQuery>("rename_saved", { id, newName }),
```

## Implementation order

1. **`src-tauri/src/history/mod.rs`** — remove the dead-code allow.
2. **`src-tauri/src/saved/mod.rs`** — remove the dead-code allow; add the
   `#[serde(rename = "scope")]` on `scope_str`.
3. **`src-tauri/src/commands/mod.rs`** — in this order:
   1. Add the `Saved` variant to `CommandError`.
   2. Add the `Display` arm for `Saved`.
   3. Add the `From<HistoryError>` and `From<SavedError>` impls.
   4. Add the six commands (with their `use` imports near the section header).
   5. Rewrite `run_query` to take `pool` and call `history::append`.
4. **`src-tauri/src/lib.rs`** — add the six commands to `generate_handler!`.
5. `( cd src-tauri && cargo build )` — clean.
6. `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` — clean.
7. `( cd src-tauri && cargo test )` — existing tests pass; M5.1's history/saved
   unit tests still pass (they don't depend on the commands).
8. **`src/lib/tauri.ts`** — add types, extend `CommandError`, add six `api.*`
   methods.
9. `pnpm check` — clean.
10. Smoke test below.

## Known gotchas

- **`pool: State<'_, sqlx::SqlitePool>`** must be added to `run_query`'s
  parameter list *before* `registry` / `results`. Tauri's macro inspects
  the order at compile time; reorder mistakes surface as a wrong-IPC-payload
  error at runtime (the JS-side argument names won't match). Keep the
  alphabetical-ish order: scalars first, `State<>` parameters in the order
  Tauri sees them (the runtime resolves them by type, not position, so
  ordering is style-only — but consistent with `list_databases`).
- **`#[serde(rename = "scope")]` on a sqlx-decoded field.** sqlx uses the
  `#[sqlx(rename = ...)]` attribute for the SQL column name and serde uses
  its own attribute for JSON output. They're independent. Without the
  serde rename, the JSON key in the wire response is the Rust field name
  (`scope_str`) — which the TS mirror would have to also call `scope_str`,
  leaking storage details to the frontend. Add both.
- **`map_query_err` reuses existing variants** (`Pg`, etc.). The history
  hook calls it on the error path, so the message recorded in history is
  the same string the frontend renders. Don't introduce a separate error
  formatter for history — divergence between history.error and the inline
  error becomes a UX bug.
- **`history::append`'s log-and-swallow.** `eprintln!` lands in Tauri's
  stderr (visible in `./run.sh` terminal output). M6 may route this through
  a structured logger; for M5 the `eprintln!` is the simplest thing that
  works.
- **`HISTORY_RETENTION as i64` cast in `list_history`.** `HISTORY_RETENTION`
  is `usize`; the SQL bind is `i64`. The cast can't overflow because
  `usize` on supported platforms is at most 64-bit and the value is 1000.
- **No CSRF / auth on these commands.** Tauri commands are local IPC;
  there's no remote attack surface. The frontend is the only caller. Don't
  bolt on auth here — it would be theatre.
- **The `start` `Instant` in `run_query` measures slightly more than the
  query module's internal `Instant`.** It includes the `slot_manager.clone()`
  and `drop(handle)` overhead. For the success path we use the more
  accurate `run.duration_ms_so_far`; for the error path we fall back to
  this outer measurement because the query module doesn't surface its
  internal duration through `QueryError`. The difference is sub-millisecond
  in practice — acceptable.
- **Don't add `history::append` inside `query::run_query`.** Two reasons:
  (1) it would force the query module to take `&SqlitePool` (a layering
  violation); (2) the error-path measurement is naturally an outer concern
  (the inner module gives up its `Instant` when it returns an error).
- **`#[tauri::command]` async functions and `State<'_, ...>` lifetimes.**
  Tauri 2 requires the explicit `'_` on `State` in async commands. The
  existing commands already do this; the new ones follow suit. Forgetting
  the `'_` produces a confusing borrow-checker error pointing at the macro
  expansion.
- **`save_query` parameter name `new` vs `newQuery`.** The Rust parameter
  is `new: NewSavedQuery`; Tauri's IPC contract uses the Rust name. The
  TS side `invoke<...>("save_query", { new: newQuery })` matches it
  exactly — note the JS key is `new`, not the local variable. Mistyping
  this is a silent failure: Tauri reports "missing argument" at runtime.
- **Reserved word `new` in JS.** It's a reserved word but valid as an
  object key (`{ new: ... }`). The TS API method takes a local parameter
  named `newQuery` to dodge the reserved-word linter and then maps it to
  the `new` key. Same pattern as `saveConnection`.
- **No new dependencies.** All work is plumbing across existing crates.
- **`#[allow(dead_code)]` is removed in M5.2** for both modules. Clippy
  will warn if any function is still unused after the wiring — if it does,
  treat it as a sign of forgotten plumbing and finish the wiring.
- **`fetch_more` and `close_result` do not append to history.** This is
  deliberate. The cursor's lifecycle is a separate concern from the
  query's history record. If you find yourself wanting to log "result
  closed" events, that's a separate `query_events` table, not history.

## Tests

`./test.sh` and `pnpm check` are the gates.

### Unit tests

No new Rust unit tests in this task — the commands are thin pass-throughs.
M5.1's history/saved unit tests already cover the underlying store calls.

The `run_query` history-hook is exercised by smoke tests (next section).
If you want a unit test, the smallest useful one is a sanity check that
`map_query_err`'s output matches what `history::append` records on the
error path — but that's already implicitly exercised by the smoke test
and adds ceremony for little gain.

### Manual smoke test

Run a local Postgres and the app:

```bash
docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17
./run.sh
```

In a separate terminal:

```bash
sqlite3 ~/.local/share/com.alberto.quill/quill.sqlite "DELETE FROM query_history; DELETE FROM saved_queries;"
```

In the app:

1. Connect to the local Postgres.
2. Run `SELECT 1` against `postgres`. Result grid shows 1 row.
3. Check the history table:
   ```bash
   sqlite3 ~/.local/share/com.alberto.quill/quill.sqlite \
     "SELECT id, sql, duration_ms, ok, error FROM query_history;"
   ```
   Expect exactly one row, `sql = 'SELECT 1'`, `ok = 1`, `error = NULL`,
   `duration_ms > 0`.
4. Run `SELECT * FROM nonexistent`. Inline error renders in the UI.
5. Recheck history — expect a second row with `ok = 0` and the error
   message in `error`. `duration_ms > 0`.
6. Run `SELECT * FROM generate_series(1, 5000)`. Click Load more once or
   twice, then Close result.
7. Recheck history — expect exactly **one** new row for this query (Load
   more / Close do not append). `duration_ms` reflects only the
   initial chunk's time, not the full streamed duration.
8. From devtools console (or a future Saved panel in M5.4):
   ```js
   await window.__TAURI__.core.invoke("save_query", {
     new: { name: "users-all", scope: "global", server_id: null, sql: "SELECT * FROM users" }
   })
   ```
   Returns a `SavedQuery` row.
9. Confirm with `sqlite3 ... "SELECT * FROM saved_queries"`.
10. Try duplicate:
    ```js
    await window.__TAURI__.core.invoke("save_query", {
      new: { name: "users-all", scope: "global", server_id: null, sql: "x" }
    })
    ```
    Expect a rejection with `kind: "Saved"`, `message` containing "already exists".
11. Disconnect and reconnect; run another query; history grows. The
    history retention cap (1000) is implicitly tested by M5.1's unit test;
    re-running 1001 queries here is unnecessary.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds — no regressions; M5.1's 8 unit tests still pass.
- [ ] `pnpm check` succeeds clean.
- [ ] `grep -n "#\\[tauri::command\\]" src-tauri/src/commands/mod.rs | wc -l`
      returns the previous count **+ 6**.
- [ ] `grep -n "history::append" src-tauri/src/commands/mod.rs` shows the
      hook in `run_query`.
- [ ] `grep -F "Saved(String)" src-tauri/src/commands/mod.rs` shows the new
      `CommandError` variant.
- [ ] `grep -F "list_history\|clear_history\|list_saved\|save_query\|delete_saved\|rename_saved" src-tauri/src/lib.rs | wc -l`
      shows all six registered.
- [ ] `grep -F "allow(dead_code)" src-tauri/src/history/mod.rs src-tauri/src/saved/mod.rs`
      returns **zero** matches.
- [ ] `grep -n "listHistory\\|listSaved\\|saveQuery\\|deleteSaved\\|renameSaved\\|clearHistory" src/lib/tauri.ts | wc -l`
      shows all six.
- [ ] Smoke step 3 — history has the success row.
- [ ] Smoke step 5 — history has the failure row with the same message
      the UI renders.
- [ ] Smoke step 7 — Load-more / Close do not create extra history rows.
- [ ] Smoke step 10 — duplicate save returns `kind: "Saved"`.

## Out of scope

- Frontend tabs — **M5.3**.
- Frontend History / Saved side panel — **M5.4**.
- CSV export — **M5.5**.
- Settings UI for `HISTORY_RETENTION` — **M6**.
- Restoring the `row_count` column to `query_history` — additive v1.1
  migration; deliberately deferred.
- Background pruning / VACUUM — never; trim runs inside `append`.
- Structured logging for the swallowed `eprintln!` — **M6**.
- Per-query latency telemetry beyond what's in `query_history` — out of
  scope; the table is the source of truth.
