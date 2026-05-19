# M3.2 — Capture cancel handles at connect; stash on each slot

## Goal

**Before (post-M3.1):** `PgConnector::connect` returns `tokio_postgres::Client`. The client carries the backend PID + secret key internally and exposes `Client::cancel_token() -> CancelToken`, but no one reads it — the value is dropped on each connect. The `Slot` struct holds only the connection, the bound database, the busy flag, the LRU timestamp, and a `disconnect_pending` flag. The `Connector` trait has no notion of cancellation.

**After:** Every Postgres connection's `CancelToken` is captured at connect time, wrapped in a small `PgCancelHandle` that remembers the SSL policy (since `CancelToken::cancel_query` opens its own TCP socket and needs the same TLS connector type), and stored on the owning slot. The `Connector` trait gains an associated `Cancel` type and `connect` returns `(Conn, Cancel)`. `SlotManager` exposes `busy_cancel_handles(&self)` so M3.3's `cancel_query` command can fire cancels against whichever slots have a query in flight — **without** acquiring a slot itself.

No new Tauri commands, no frontend changes, no actual user-visible cancellation yet. That all lands in M3.3. M3.2 is the plumbing: the moment a connection becomes live, its cancel credentials are wired into a place where M3.3 can find them.

## Current state

### `src-tauri/src/slots/mod.rs` (post-M1.3, unchanged by M3.1)

The relevant types as they exist today:

```rust
#[async_trait]
pub trait Connector: Send + Sync + 'static {
    type Conn: Send + 'static;
    async fn connect(&self, database: &str) -> Result<Self::Conn, ConnectorError>;
    async fn close(conn: Self::Conn);
}

struct Slot<C: Connector> {
    conn: Option<C::Conn>,
    database: Option<String>,
    last_used: Instant,
    busy: bool,
    disconnect_pending: bool,
}

enum SlotDecision<C: Connector> {
    Reuse { idx: usize, conn: C::Conn },
    NeedsConnect { idx: usize, evict_conn: Option<C::Conn> },
}
```

`apply_rules` mutates a slot to "almost-bound" state (sets `database`, `busy`, returns a decision); the async I/O happens outside the lock; `Recovery` cleans up on cancellation.

### `src-tauri/src/pg/mod.rs` (post-M3.1)

`PgConnector::connect` returns `Client`. `SslPolicy` is parsed from text. `make_rustls()` builds a `MakeRustlsConnect` for SSL-mode-`require`-and-above.

### `src-tauri/src/registry.rs`

`ServerHandle { slot_manager: Arc<SlotManager<PgConnector>> }`. M3.3 will call methods on `ServerHandle` to dispatch cancels.

### `src-tauri/src/commands/mod.rs`

`run_query` and the four introspect-driven commands acquire slots via `slot_manager.acquire(&db)`. No command currently reads cancel state.

## Design choices baked into this spec

- **`Connector::Cancel` is an associated type, not a fixed shape.** The slot manager stays connector-agnostic; the actual cancel mechanism lives in `pg/mod.rs`. `FakeConnector` in tests uses a trivial `Cancel = Arc<AtomicUsize>` (counter) so we can assert cancels were even *retrievable*.
- **`Cancel: Clone + Send + Sync + 'static`.** Multiple consumers may want to dispatch the same cancel (rare, but the type cost is low — `Arc<...>` for the real impl).
- **Cancel handle lives on the `Slot`, not on the `SlotGuard`.** A guard is loaned out for the duration of a query; the cancel must be accessible *while* the guard is held. Storing on the slot itself solves this — the lock-protected slot vector is exactly where to look.
- **Cancel survives reuse, dies with eviction/close.** Rule 1 (reuse) keeps the existing cancel. Rule 3 (LRU evict) drops the old cancel along with the old conn before opening a new one. `disconnect_all` clears all cancels (busy slots clear theirs on guard drop, idle clears immediately).
- **`PgCancelHandle` carries `SslPolicy`.** `CancelToken::cancel_query` requires a `MakeTlsConnect`. The handle remembers which policy this connection used so the cancel socket is opened with matching TLS settings. M3.3 builds the connector inside the handle's `cancel()` method.
- **Cancel is *not* part of the principle-1 "explicit user action" budget.** Sending a CancelRequest opens a one-shot TCP connection that completes in milliseconds and is closed before the call returns. It does not count as a slot. AGENTS.md principle 1 forbids *background* connections — cancels are foreground.

## Deliverables

### 1. `src-tauri/src/slots/mod.rs` — expand `Connector` and `Slot`

Trait change (additive — new associated type, new tuple return):

```rust
#[async_trait]
pub trait Connector: Send + Sync + 'static {
    type Conn: Send + 'static;

    /// A cheap-to-clone handle that, when invoked, terminates the in-flight
    /// query on this connection out-of-band.  See [`crate::pg::PgCancelHandle`]
    /// for the Postgres impl.  The slot manager treats this value as opaque
    /// — it only stores, clones, and hands it out.
    type Cancel: Clone + Send + Sync + 'static;

    async fn connect(
        &self,
        database: &str,
    ) -> Result<(Self::Conn, Self::Cancel), ConnectorError>;

    async fn close(conn: Self::Conn);
}
```

Augment `Slot`:

```rust
struct Slot<C: Connector> {
    conn: Option<C::Conn>,
    database: Option<String>,
    last_used: Instant,
    busy: bool,
    disconnect_pending: bool,
    /// Cancel handle for the connection currently bound to this slot.
    /// `Some` iff `database` is `Some` AND the slot has reached the "live
    /// connection" state at least once.  Cleared on close/eviction.
    cancel: Option<C::Cancel>,
}
```

`Slot::free()` initializes `cancel: None`.

Augment `SlotDecision::NeedsConnect` to carry the evicted slot's cancel handle (so it gets dropped together with the evicted conn — and any future use of the cancel after eviction is impossible at the type level):

```rust
enum SlotDecision<C: Connector> {
    Reuse { idx: usize, conn: C::Conn },
    NeedsConnect {
        idx: usize,
        evict_conn: Option<C::Conn>,
        // No need to surface evict_cancel — it's already detached from the
        // slot when we return.  Document this in apply_rules.
    },
}
```

### 2. Update `apply_rules` to manage the cancel slot field

Three changes:

- **Rule 1 (reuse):** no change to cancel. The slot already has `cancel = Some(...)`.
- **Rule 2 (bind free):** no change yet — the cancel doesn't exist until `connect` returns. Slot becomes `busy=true, database=Some(db), conn=None, cancel=None`.
- **Rule 3 (evict LRU):** before reassigning the slot, take **both** `slot.conn` *and* `slot.cancel`. Drop the cancel (it's invalid once the conn is closed). Reset `cancel = None` on the slot.

After `apply_rules` returns `NeedsConnect`, the slot is in the "reserved" state with `cancel=None`. The `acquire` future then awaits the connector's `connect`, gets `(Conn, Cancel)`, and writes both into the slot via a small helper.

### 3. Update `SlotManager::acquire`

Phase 1 unchanged (lock + `apply_rules`).

Phase 2 (`SlotDecision::NeedsConnect`): close evicted conn, then `connector.connect(&db).await?` returns `(new_conn, new_cancel)`. Take the lock again briefly and write the cancel into the slot:

```rust
SlotDecision::NeedsConnect { idx, evict_conn } => {
    if let Some(old_conn) = evict_conn {
        C::close(old_conn).await;
    }

    let recovery = Recovery {
        slots: &self.slots,
        idx,
        recovered: std::cell::Cell::new(false),
    };

    let (new_conn, new_cancel) = self.connector.connect(&db).await?;

    // Slot's cancel is set here; it persists across busy/idle transitions
    // until eviction or disconnect_all.
    {
        let mut slots = self.slots.lock().unwrap();
        slots[idx].cancel = Some(new_cancel);
    }

    recovery.recovered.set(true);

    Ok(SlotGuard {
        manager: self,
        slot_idx: idx,
        conn: Some(new_conn),
    })
}
```

**Important**: the cancel is written to the slot, **not** to the guard. The guard still owns the conn for the query lifetime; the cancel is in the slot so external callers (M3.3) can read it while the slot is busy.

`Recovery::drop` on cancelled `acquire` also clears `slot.cancel = None` for symmetry — should be no-op since we hadn't set it yet, but explicit is safer.

### 4. Update `SlotManager::disconnect_all` and `SlotGuard::drop`

`disconnect_all`:

- Idle slots: take both `conn` and `cancel`, drop the cancel, close the conn.
- Busy slots: set `disconnect_pending = true`. **Do not clear `cancel`** — the in-flight query can still be cancelled out-of-band during the brief window before the guard drops. The guard's drop clears it.

`SlotGuard::drop`:

- Normal path: conn returned to slot. **Cancel stays put.**
- `disconnect_pending = true` path: conn taken for async close; clear `slot.cancel = None`; clear `slot.database = None`.

### 5. Add `SlotManager::busy_cancel_handles`

Public accessor used by M3.3:

```rust
impl<C: Connector> SlotManager<C> {
    /// Return a cloned cancel handle for every slot that is currently
    /// **busy** (i.e. has a guard outstanding and therefore a query in
    /// flight).  Optionally filter by database.
    ///
    /// Used by the `cancel_query` Tauri command to dispatch
    /// `CancelRequest` packets without touching any slot — see M3.3 spec.
    pub fn busy_cancel_handles(&self, database: Option<&str>) -> Vec<C::Cancel> {
        let slots = self.slots.lock().unwrap();
        slots
            .iter()
            .filter(|s| s.busy)
            .filter(|s| match database {
                Some(d) => s.database.as_deref() == Some(d),
                None => true,
            })
            .filter_map(|s| s.cancel.clone())
            .collect()
    }
}
```

**Important**: this method intentionally takes a snapshot under the lock and drops the lock before returning. Callers may then `.await` on each handle's `.cancel()` without holding the slot lock.

### 6. `src-tauri/src/pg/mod.rs` — define `PgCancelHandle` and update `connect`

Add the cancel handle type at the bottom of the file:

```rust
use std::sync::Arc;
use tokio_postgres::CancelToken;

/// Cancel handle for a single Postgres backend.
///
/// Carries the [`CancelToken`] (which embeds PID + secret key from the
/// `BackendKeyData` startup message) and the [`SslPolicy`] used by the
/// originating connection — `cancel_query` opens its own TCP socket and
/// must match the SSL setup of the original handshake.
///
/// Wrapped in [`Arc`] so the slot manager can clone it cheaply for hand-off
/// to the `cancel_query` Tauri command.
#[derive(Clone)]
pub struct PgCancelHandle {
    inner: Arc<PgCancelInner>,
}

struct PgCancelInner {
    token: CancelToken,
    ssl_policy: SslPolicy,
}

impl PgCancelHandle {
    fn new(token: CancelToken, ssl_policy: SslPolicy) -> Self {
        Self {
            inner: Arc::new(PgCancelInner { token, ssl_policy }),
        }
    }

    /// Send a `CancelRequest` over a fresh TCP connection.  Returns the
    /// underlying tokio-postgres error message on failure; the cancel is
    /// best-effort — Postgres returns no acknowledgement.
    pub async fn cancel(&self) -> Result<(), String> {
        let inner = &self.inner;
        if inner.ssl_policy.wants_tls() {
            let tls = make_rustls().map_err(|e| format!("rustls setup failed: {e}"))?;
            inner
                .token
                .cancel_query(tls)
                .await
                .map_err(|e| e.to_string())
        } else {
            inner
                .token
                .cancel_query(NoTls)
                .await
                .map_err(|e| e.to_string())
        }
    }
}
```

Update the `Connector` impl:

```rust
#[async_trait]
impl Connector for PgConnector {
    type Conn = Client;
    type Cancel = PgCancelHandle;

    async fn connect(
        &self,
        database: &str,
    ) -> Result<(Self::Conn, Self::Cancel), ConnectorError> {
        let config = self.build_config(database);

        let (client, cancel_token) = if self.ssl_mode.wants_tls() {
            let tls = make_rustls()
                .map_err(|e| ConnectorError(format!("rustls setup failed: {e}")))?;
            let (client, connection) = config
                .connect(tls)
                .await
                .map_err(|e| ConnectorError(e.to_string()))?;
            let cancel_token = client.cancel_token();
            spawn_connection_driver(connection);
            (client, cancel_token)
        } else {
            let (client, connection) = config
                .connect(NoTls)
                .await
                .map_err(|e| ConnectorError(e.to_string()))?;
            let cancel_token = client.cancel_token();
            spawn_connection_driver(connection);
            (client, cancel_token)
        };

        Ok((client, PgCancelHandle::new(cancel_token, self.ssl_mode)))
    }

    async fn close(_conn: Self::Conn) {
        // intentionally empty (see M3.1)
    }
}
```

### 7. Update the `FakeConnector` in `slots::tests`

Add a fake cancel type and verify it's preserved across reuse and dropped on eviction:

```rust
#[derive(Clone, Default)]
struct FakeCancel {
    cancel_count: Arc<AtomicUsize>,
}

#[async_trait]
impl Connector for FakeConnector {
    type Conn = FakeConn;
    type Cancel = FakeCancel;

    async fn connect(
        &self,
        _database: &str,
    ) -> Result<(Self::Conn, Self::Cancel), ConnectorError> {
        self.connect_counter.fetch_add(1, Ordering::SeqCst);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let cancel = FakeCancel::default();
        let conn = FakeConn {
            _id: id as u32,
            close_counter: self.close_counter.clone(),
        };
        Ok((conn, cancel))
    }

    async fn close(conn: Self::Conn) {
        conn.close_counter.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}
```

Likewise for `HangingConnector::Cancel = ()` — the test never reaches the cancel-bind step.

Add two new tests:

```rust
#[tokio::test]
async fn busy_cancel_handles_returns_one_per_busy_slot() {
    let (conn, _connects, _closes) = FakeConnector::new();
    let mgr = SlotManager::new(conn, 2);

    let g1 = mgr.acquire("A").await.unwrap();
    let g2 = mgr.acquire("B").await.unwrap();

    let handles = mgr.busy_cancel_handles(None);
    assert_eq!(handles.len(), 2);

    let only_a = mgr.busy_cancel_handles(Some("A"));
    assert_eq!(only_a.len(), 1);

    drop(g1);
    drop(g2);

    // After guards drop, slots are idle — busy_cancel_handles returns empty.
    assert!(mgr.busy_cancel_handles(None).is_empty());
}

#[tokio::test]
async fn lru_eviction_drops_old_cancel() {
    let (conn, _connects, _closes) = FakeConnector::new();
    let mgr = SlotManager::new(conn, 1);

    // Bind to A.
    let g = mgr.acquire("A").await.unwrap();
    drop(g);

    // Acquire B, which evicts A.
    let g = mgr.acquire("B").await.unwrap();
    drop(g);

    // The slot's cancel must now be the one tied to B, not A.  We can't
    // observe identity directly, but we can observe that there is exactly
    // one cancel handle reachable.
    let g = mgr.acquire("B").await.unwrap();
    let handles = mgr.busy_cancel_handles(None);
    drop(g);
    assert_eq!(handles.len(), 1);
}
```

### 8. `src-tauri/src/registry.rs` — no signature changes

`ServerHandle::new(connector, budget)` stays. The `SlotManager<PgConnector>` parameterization picks up the new `Cancel = PgCancelHandle` associated type automatically.

### 9. `src-tauri/src/commands/mod.rs` — no surface changes

`run_query` still calls `slot_manager.acquire(&db)`. The acquire path internally captures the cancel; the command path doesn't read it. **M3.3 will add the `cancel_query` command on top of this scaffolding.**

## Implementation order

1. **`slots/mod.rs`** — trait + `Slot` + `SlotDecision` + `apply_rules` + `acquire` + `disconnect_all` + `SlotGuard::drop` + `busy_cancel_handles`. `cargo build` will fail because `FakeConnector` and `PgConnector` no longer satisfy the trait.
2. **`slots/mod.rs` tests** — update `FakeConnector` (and the `HangingConnector`) to satisfy the new trait. Add the two new tests. `cargo test -p quill_lib --lib slots` should pass.
3. **`pg/mod.rs`** — add `PgCancelHandle`, update `Connector` impl to return `(Client, PgCancelHandle)`. `cargo build` should now succeed clean.
4. **Run the full test + smoke suite** — `./test.sh`, then `./run.sh` and a manual reconnect/refresh cycle.

## Known gotchas

- **`CancelToken: Clone` is `pub` in tokio-postgres** — confirm with the v0.7.x docs. The clone is cheap (PID + secret key + Config snapshot). Our `Arc` wrap is for the SSL policy and any future fields, not because `CancelToken` itself is expensive.
- **`PgCancelInner` does not derive Debug.** `CancelToken` doesn't impl Debug. Don't try to derive it on the inner. If anything in `commands/` needs to log it, add a manual `Debug` impl that prints `"<cancel handle for ...>"` without the credentials.
- **`busy_cancel_handles` holds the lock briefly.** The lock is dropped before any handle's `cancel()` is awaited. Don't be tempted to combine "snapshot" and "dispatch" in one function — that re-introduces holding-lock-across-await.
- **Locking inside `acquire`'s phase 2.** After connect, we re-take the lock to write the cancel. This is OK because we're not holding it across the connect's `await`. The recovery guard is the only thing protecting the slot during connect; the cancel-set step happens *after* recovery has been marked recovered.
- **`SlotGuard::drop` runs in the destructor — no async.** Clearing `slot.cancel = None` is a synchronous field write. The async close of the conn (when `disconnect_pending`) is `tokio::spawn`'d; it doesn't need access to the cancel.
- **Cancel doesn't follow conn into guards.** If the connection is loaned out via a guard, the slot still has `cancel` set. This is intentional: cancellation works *during* a query, which is exactly when the guard is alive.
- **`disconnect_all` busy-slot cancels stay live during the closing window.** If a user cancels a query mid-disconnect, the cancel may race with the conn closing. Both are safe to call; whichever finishes first wins. Document this in `disconnect_all`'s doc comment.
- **Eviction in Rule 3 must clear `cancel` BEFORE the new connect.** If `connect` then fails, the slot ends up free (via `Recovery`) — and `Recovery::drop` writes `cancel = None` for belt-and-braces. Without the early clear, a failed connect on an evicted slot could leave the previous DB's cancel lingering.
- **No `Connector::Cancel = ()` for a "no cancel" connector.** Real Postgres always has cancellation. The associated type is a hard requirement; no default. `FakeConnector` in tests picks a non-`()` type so the wire-up is exercised.
- **Cancel handle clone count is unbounded.** Frontend invokes `cancel_query` rapidly (M3.3 will), each clone is an Arc bump. That's fine — Arcs are cheap. Don't introduce a "cancel once" semantic at the slot level; the M3.3 command will call `cancel()` and that's that.
- **No new `tokio-postgres` features needed.** `CancelToken` is in the default tokio-postgres feature set; no opt-in. Confirm with `cargo doc --open --no-deps -p tokio-postgres` if unsure.
- **Avoid `Connector::Cancel: Sync`?** It already needs to be `Sync` because the slot vec is behind a `Mutex<Vec<Slot<C>>>` and we read `Slot::cancel` under that lock. The bound is required.

## Tests

- **Existing slot-manager tests** continue to pass (rule 1–4, concurrency, set_budget, disconnect_all, cancelled_acquire_restores_slot, state_reflects_current_slots). The `FakeConnector` update is the only diff.
- **New: `busy_cancel_handles_returns_one_per_busy_slot`** — verifies the snapshot semantic and the database filter.
- **New: `lru_eviction_drops_old_cancel`** — verifies cancel handle lifecycle across eviction.
- **`pg/mod.rs`** has no easy unit-test path for `PgCancelHandle::cancel` (it opens a TCP connection). Leave it untested at the unit level; M3.3's smoke test against `pg_sleep(10)` covers it end-to-end.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds.
- [ ] `cargo test -p quill_lib --lib slots` reports **at least 11 passing tests** (the 9 from M1 + 2 from M3.2).
- [ ] `grep -RIn 'busy_cancel_handles' src-tauri/src/slots/mod.rs` returns one impl + zero callers (the caller lands in M3.3).
- [ ] `grep -RIn 'PgCancelHandle' src-tauri/src` returns matches in `pg/mod.rs` only.
- [ ] `grep -F 'type Cancel' src-tauri/src/slots/mod.rs` returns matches in both the trait and the `FakeConnector` impl.
- [ ] `git diff src/` is empty (frontend untouched).
- [ ] `git diff src-tauri/migrations/` is empty.
- [ ] Smoke test: `./run.sh`, connect, run a fast query and a slow query — both succeed; nothing new is visible in the UI. (Cancellation is wired in M3.3.)

## Out of scope

- The `cancel_query` Tauri command — **M3.3**.
- Frontend Cancel button — **M3.6**.
- Tracking *which* query is in flight at the slot level (just "busy" + "cancel handle" is enough) — multi-statement / per-statement cancel tracking is a v1.1 problem.
- Persisting the cancel handle across reconnects — handles die with the connection; the registry rebuilds them on reconnect.
- Exposing cancel handle counts in `SlotState` — UI doesn't need them in v1 (handle count is implicit from busy count).
- A trait-level `Cancel` method on `SlotManager` itself — `busy_cancel_handles` returns clones; callers `.await` them directly. Adding `SlotManager::cancel_all(&self)` would push policy into the slot manager that belongs in the command layer.
