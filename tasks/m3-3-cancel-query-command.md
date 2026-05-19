# M3.3 — `cancel_query` Tauri command (out-of-band cancellation)

## Goal

**Before (post-M3.2):** Each `Slot` carries a `PgCancelHandle` (cloneable, Arc-backed wrapper around `tokio_postgres::CancelToken` + `SslPolicy`). `SlotManager::busy_cancel_handles(database)` returns clones of every cancel handle whose slot is currently busy. **Nothing calls it.** The frontend has no way to cancel a running query.

**After:** A new `#[tauri::command] async fn cancel_query(server_id: i64, database: Option<String>)` looks up the server's `SlotManager`, snapshots the cancel handles for whatever query is in flight (optionally filtered by database), and dispatches every cancel **concurrently** via `tokio::join!`-style fan-out. **The command does not acquire a slot.** It opens a fresh out-of-band TCP connection per handle — the cancel arrives at Postgres and the running query returns a "query was cancelled by user" error to the caller of `run_query`. The frontend `src/lib/tauri.ts` gains a `cancelQuery(serverId, database?)` method. No UI button yet (M3.6 wires that).

Acceptance is end-to-end: connect, run `SELECT pg_sleep(10)`, fire `cancel_query` from a second client (or via a manual invoke), and the `run_query` Promise rejects within ~1 second with `kind: "Pg"` and `message` containing `"canceling statement"`.

## Current state

### `src-tauri/src/slots/mod.rs` (post-M3.2)

```rust
pub fn busy_cancel_handles(&self, database: Option<&str>) -> Vec<C::Cancel>
```

### `src-tauri/src/pg/mod.rs` (post-M3.2)

```rust
#[derive(Clone)]
pub struct PgCancelHandle { /* Arc<PgCancelInner> */ }

impl PgCancelHandle {
    pub async fn cancel(&self) -> Result<(), String> { /* ... */ }
}
```

### `src-tauri/src/commands/mod.rs` (post-M3.1)

12 commands, none of which read cancel state. `CommandError` has variants `UnknownConnection`, `NotConnected`, `Slot`, `Pg`, `Store`, `Introspect`, `UnknownDatabase`.

### `src-tauri/src/lib.rs`

`invoke_handler` lists 12 commands today; this task adds a 13th.

### `src/lib/tauri.ts`

12 methods on `api`; this task adds a 13th.

## Design choices baked into this spec

- **Cancel is a fan-out, not a single handle lookup.** If two slots are busy (e.g. introspect + a user query running in parallel), we cancel both. Filtering by database is supported but defaults to "cancel all". The frontend usually passes the active database; introspect typically completes faster than a user-driven slow query anyway.
- **No "what query was running" tracking in v1.** Cancel is fire-and-forget across all busy slots that match the filter. A future per-statement tracking system (v1.1) could cancel only the user's tab's query, but it would require a query-id registry that lives outside the scope of the slot manager.
- **Cancel does not consume a slot.** It opens its own TCP socket via `CancelToken::cancel_query`, sends the 16-byte `CancelRequest` packet, and closes — no slot, no `acquire` call, no LRU eviction, no budget impact. Slot indicator stays unchanged during cancel.
- **Cancel runs all dispatches concurrently.** `futures::future::join_all` (or hand-rolled `tokio::spawn` + `JoinSet`) — not sequential. Slow servers shouldn't make a single cancel-all wait sum-of-RTTs.
- **Cancel result aggregates errors.** If 2 of 3 dispatches succeed and one fails, we return success with a `cancelled` count and an `errors` list. The frontend reports failures inline (or silently in v1).
- **Cancel returns metadata, not just `()`.** The frontend wants to know "how many queries did this hit" so it can show a transient "cancelled 1 query" indicator (M3.6). The shape is `CancelOutcome { cancelled: usize, errors: Vec<String> }`.
- **Idempotent / safe to spam.** Calling cancel when nothing is running returns `cancelled: 0`, no error. Calling cancel twice on the same in-flight query is harmless — the second cancel either races and is no-op, or races and arrives after Postgres already returned the cancel error.

## Deliverables

### 1. `src-tauri/src/commands/mod.rs` — add the command

Add at the bottom of the file, just before `#[cfg(test)] mod tests`:

```rust
// ═══════════════════════════════════════════════════════════════════════════
// Cancellation
// ═══════════════════════════════════════════════════════════════════════════

/// Outcome of a `cancel_query` invocation.  `cancelled` counts how many
/// CancelRequest packets we successfully dispatched; `errors` collects any
/// per-handle failures.  v1 surfaces `errors` for debugging but the
/// frontend can ignore them.
#[derive(Debug, Serialize)]
pub struct CancelOutcome {
    pub cancelled: usize,
    pub errors: Vec<String>,
}

/// Cancel every in-flight query on `server_id`, optionally filtered to
/// queries running against `database`.
///
/// Does **not** acquire a slot.  Each cancel opens a fresh TCP connection
/// to the server, sends the Postgres CancelRequest packet, and closes —
/// AGENTS.md principle 1 still holds because the cancel is a *direct
/// consequence of an explicit user action* (the Cancel button).
#[tauri::command]
pub async fn cancel_query(
    server_id: i64,
    database: Option<String>,
    registry: State<'_, ServerRegistry>,
) -> Result<CancelOutcome, CommandError> {
    let handle = registry
        .by_id
        .get(&server_id)
        .ok_or_else(|| CommandError::not_connected(server_id))?;

    let slot_manager = handle.slot_manager.clone();
    drop(handle); // release DashMap shard lock before awaiting

    let handles = slot_manager.busy_cancel_handles(database.as_deref());

    // Empty fan-out is a no-op success — easier for the frontend than an error.
    if handles.is_empty() {
        return Ok(CancelOutcome {
            cancelled: 0,
            errors: Vec::new(),
        });
    }

    // Run every cancel concurrently.
    let mut tasks = tokio::task::JoinSet::new();
    for h in handles {
        tasks.spawn(async move { h.cancel().await });
    }

    let mut cancelled = 0usize;
    let mut errors: Vec<String> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(())) => cancelled += 1,
            Ok(Err(msg)) => errors.push(msg),
            Err(join_err) => errors.push(format!("task panicked: {join_err}")),
        }
    }

    Ok(CancelOutcome { cancelled, errors })
}
```

Notes:

- The `tokio::task::JoinSet` import lives in `tokio::task`. No new dep.
- `database.as_deref()` converts `Option<String>` → `Option<&str>` to match the `busy_cancel_handles` signature.
- The pattern of "snapshot handles under lock, then dispatch outside lock" was decided in M3.2 — don't re-litigate it here.

### 2. `src-tauri/src/lib.rs` — register the command

Add `commands::cancel_query` to `invoke_handler`:

```rust
.invoke_handler(tauri::generate_handler![
    commands::list_connections,
    commands::save_connection,
    commands::delete_connection,
    commands::connect_server,
    commands::disconnect_server,
    commands::run_query,
    commands::get_slot_state,
    commands::list_databases,
    commands::list_schemas,
    commands::list_relations,
    commands::list_functions,
    commands::refresh_schema_cache,
    commands::cancel_query, // M3.3
])
```

### 3. `src/lib/tauri.ts` — add the type and method

Append after the `SchemaPayload` block, before the `CommandError` declaration:

```ts
// ── Cancellation (mirrors commands::CancelOutcome) ──

export type CancelOutcome = {
  cancelled: number;
  errors: string[];
};
```

Add the method inside `api`, after `refreshSchemaCache`:

```ts
  cancelQuery: (serverId: number, database: string | null = null) =>
    invoke<CancelOutcome>("cancel_query", { serverId, database }),
```

Note: the Tauri side declares `database: Option<String>`; serde-tauri maps `null` from JS to `None`. Pass `null` for "cancel all"; pass a string to filter.

### 4. (Optional) `src-tauri/src/commands/mod.rs` — add a unit test

There's nothing easy to unit-test on the command itself (it depends on a live registry and live Postgres). Skip in favor of the smoke test below.

## Implementation order

1. Add the `CancelOutcome` struct and `cancel_query` command in `commands/mod.rs`.
2. Register the command in `lib.rs`.
3. Add the type and method in `src/lib/tauri.ts`.
4. `cargo build` + `pnpm check` — both must succeed clean.
5. Smoke test (see below).

## Known gotchas

- **`JoinSet` is in `tokio::task`, not `tokio`.** `use tokio::task::JoinSet;` if you prefer a path import. The example uses fully-qualified.
- **The cancel TCP connection inherits the SSL state of the original.** `PgCancelHandle::cancel` picks `NoTls` or `MakeRustlsConnect` per `SslPolicy` — that's why M3.2 stashed the policy.
- **Cancel is a best-effort wire signal.** Postgres' `CancelRequest` returns no acknowledgement; the only confirmation is that the running `run_query` errors out with `"canceling statement due to user request"`. Don't try to "verify" the cancel inside the command itself — just dispatch and return.
- **`Option<String>` Tauri marshalling.** Tauri 2's serde glue maps `undefined` / `null` from JS to `None`. If the frontend ever passes the literal string `"null"` (it shouldn't), Postgres would interpret it as a database named "null" and the filter would match nothing — `cancelled = 0`.
- **`busy_cancel_handles` is the source of truth for "what to cancel".** Don't add a side-channel "last-issued query" registry; that's v1.1.
- **Permission errors are out of scope.** Cancellation only works against the same backend that opened the connection; pg_terminate_backend is a privileged variant we are not using. The user already authenticated as themself.
- **Slot indicator must not flicker.** `cancel_query` doesn't acquire a slot. Verify in the smoke test below: while the cancel is dispatching, `[1/2]` stays at `[1/2]` — does *not* bump to `[2/2]`.
- **Two concurrent `cancel_query` calls.** The fan-out is safe to overlap — `CancelToken` is cheap to clone, and Postgres ignores duplicate cancels (the second arrives after the backend is already cancelled and is a no-op).
- **Frontend timing.** `cancel_query` typically resolves in 5–50 ms (single round-trip over TCP). The frontend Promise that called `run_query` rejects ~RTT after that. There is no callback / event from cancel — observe the `run_query` rejection.
- **`CancelOutcome.errors` is informational.** v1 frontend can ignore it; M6 polish can render it in a transient toast.
- **No new error variants.** `CommandError::NotConnected` already covers "no server with this id." If `busy_cancel_handles` returns empty, that's not an error — it's `Ok(CancelOutcome { cancelled: 0, errors: [] })`.

## Tests

### Manual smoke test (requires a real Postgres)

Run a fresh Postgres in Docker:

```bash
docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17
```

In a terminal session:

```bash
./run.sh
```

In the Quill UI:
1. Add connection (host=localhost, port=5432, default_db=postgres, username=postgres, password=dev).
2. Connect.
3. Click the database node `postgres` in the tree.
4. In the right pane SQL textarea, type:
   ```sql
   SELECT pg_sleep(10);
   ```
5. Click Run. Slot indicator shows `[1/2]`.
6. **While the query is running**, open the browser devtools and run in the console:
   ```javascript
   await window.__TAURI_INTERNALS__.invoke("cancel_query", { serverId: 1, database: null });
   ```
   (Replace `serverId` with whatever the saved connection's id is.)
7. The original `run_query` Promise rejects with `{ kind: "Pg", message: "...canceling statement due to user request..." }`.
8. Slot indicator returns to `[0/2]` (or `[0/2]` if it stayed there during cancel).
9. The console call resolves to `{ cancelled: 1, errors: [] }`.
10. Re-run the query — must complete normally. The cancel did not break the slot.

### Negative paths to verify manually

- Call `cancel_query` when no query is running → `{ cancelled: 0, errors: [] }`. No error.
- Call `cancel_query` against a server id that isn't connected → rejects with `kind: "NotConnected"`.
- Call with a database filter that matches nothing → `{ cancelled: 0, errors: [] }`.

### Optional integration test

`src-tauri/tests/pg_cancel.rs` (new file, `#[ignore]` gated by `QUILL_TEST_PG_URL`):

```rust
//! End-to-end cancel-query integration test.  Gated by env var because
//! it requires a live Postgres server.
//!
//! Run with:
//!   QUILL_TEST_PG_URL=postgres://postgres:dev@localhost:5432/postgres \
//!     cargo test -p quill_lib --test pg_cancel -- --ignored

use std::time::Duration;

use quill_lib::pg::PgConnector;
use quill_lib::slots::SlotManager;
use secrecy::SecretString;
use tokio::time::timeout;
use url::Url;

#[tokio::test]
#[ignore = "requires QUILL_TEST_PG_URL"]
async fn cancel_query_interrupts_pg_sleep() {
    let url = std::env::var("QUILL_TEST_PG_URL").expect("set QUILL_TEST_PG_URL");
    let url = Url::parse(&url).unwrap();

    let connector = PgConnector {
        host: url.host_str().unwrap().to_string(),
        port: url.port().unwrap_or(5432),
        username: url.username().to_string(),
        password: SecretString::from(url.password().unwrap_or("").to_string()),
        ssl_mode: PgConnector::parse_ssl_mode("disable").unwrap(),
    };

    let mgr = std::sync::Arc::new(SlotManager::new(connector, 2));
    let db = url.path().trim_start_matches('/').to_string();

    let mgr_for_query = mgr.clone();
    let db_for_query = db.clone();
    let query_task = tokio::spawn(async move {
        let guard = mgr_for_query.acquire(&db_for_query).await.unwrap();
        guard.query("SELECT pg_sleep(60)", &[]).await
    });

    // Give the query 200 ms to actually start before cancelling.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let handles = mgr.busy_cancel_handles(None);
    assert_eq!(handles.len(), 1, "exactly one busy slot expected");

    handles[0].cancel().await.unwrap();

    let result = timeout(Duration::from_secs(5), query_task).await.unwrap().unwrap();
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("cancel"),
        "expected cancel error, got: {err}"
    );
}
```

This is **optional**; the manual smoke test is the contract for M3.3 acceptance. If the integration test is added, mark it `#[ignore]` and document the env var in the test file header.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds (existing tests unaffected).
- [ ] `pnpm check` succeeds.
- [ ] `grep -F 'commands::cancel_query' src-tauri/src/lib.rs` returns exactly one match.
- [ ] `grep -F 'cancelQuery' src/lib/tauri.ts` returns matches in both the `api` block and any future caller. (At this task's end, only the `api` definition uses it.)
- [ ] `grep -c '#\[tauri::command\]' src-tauri/src/commands/mod.rs` returns `13`.
- [ ] Smoke test: cancelling `SELECT pg_sleep(10)` from devtools rejects the original `run_query` within ~1 second; result is `{ cancelled: 1, errors: [] }`.
- [ ] Slot indicator does **not** bump during a `cancel_query` call (verified visually during smoke test).
- [ ] Subsequent queries on the same slot work normally after a cancel.
- [ ] No frontend UI is added in this task (the Cancel button is M3.6).
- [ ] `git diff src-tauri/migrations/` is empty.

## Out of scope

- A user-visible Cancel button — **M3.6**.
- Per-query / per-tab cancel tracking — v1.1.
- Privileged termination (`pg_terminate_backend`) — out of scope; v1 uses only same-user cancel.
- Surfacing `CancelOutcome.errors` in the UI — **M6** polish (transient toast).
- Implementing the Postgres CancelRequest packet by hand — already done for us by `tokio_postgres::CancelToken::cancel_query`. M1's milestone hint about hand-rolling the 16-byte packet is moot now that M3.1 swapped clients.
- Cancellation during `disconnect_all` — out of scope; M3.2 already documented that the cancel handle stays live during the close window, but the user-facing semantic is "disconnect closes everything anyway, so cancel is redundant."
- Streaming-query cancellation semantics (open cursor + cancel mid-fetch) — that interaction is **M3.4**, which decides how `close_result` and cancel coexist.
