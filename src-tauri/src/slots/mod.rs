//! Connection slot manager — enforces "no more than *N* live connections per server."
//!
//! This is the heart of Quill's reason to exist: strict, user-visible control over
//! the number of active connections per server.  See PRD §6 and AGENTS.md principles
//! 1 and 2.
//!
//! ## Design
//!
//! - Every live connection is owned by exactly one slot; the number of slots is the
//!   maximum concurrent connections to that server.
//! - The slot vector is behind a `std::sync::Mutex` because all work done while
//!   holding the lock is synchronous (searching, field updates, moving connections).
//!   The only async operations — connect / close — happen **after** the lock is dropped.
//! - Cancellation safety: if the `acquire` future is cancelled or errors after a
//!   slot has been reserved, the slot is restored to a free / unbound state.
//! - No keepalives, no background tasks, no pre-fetching (AGENTS.md principle 1).

use std::ops::Deref;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

// ═══════════════════════════════════════════════════════════════════════════
// Public types
// ═══════════════════════════════════════════════════════════════════════════

/// Errors from the connector when establishing a connection.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ConnectorError(pub String);

/// A trait for creating and destroying database connections.
///
/// `close` is best-effort and may swallow errors.
#[async_trait]
pub trait Connector: Send + Sync + 'static {
    type Conn: Send + 'static;

    /// A cheap-to-clone handle that, when invoked, terminates the in-flight
    /// query on this connection out-of-band.  See [`crate::pg::PgCancelHandle`]
    /// for the Postgres impl.  The slot manager treats this value as opaque
    /// — it only stores, clones, and hands it out.
    type Cancel: Clone + Send + Sync + 'static;

    /// Open a new connection to `database`.
    async fn connect(&self, database: &str) -> Result<(Self::Conn, Self::Cancel), ConnectorError>;

    /// Close a previously-opened connection (best-effort).
    async fn close(conn: Self::Conn);
}

/// Errors returned by [`SlotManager`] operations.
#[derive(Debug, Error)]
pub enum SlotError {
    /// All slots on the server are currently busy.
    #[error("no slot available; all {0} slots are busy")]
    AllBusy(usize),

    /// Shrinking the budget is not yet supported (M1 limitation).
    #[error("cannot shrink slot budget (M1 limitation)")]
    CannotShrink,

    /// The connector failed to establish a new connection.
    #[error("connect failed: {0}")]
    Connect(#[from] ConnectorError),
}

/// Current state of all slots — for UI consumption.
#[derive(Debug, Clone, Serialize)]
pub struct SlotState {
    pub budget: usize,
    pub slots: Vec<SlotInfo>,
}

/// Information about a single slot.
#[derive(Debug, Clone, Serialize)]
pub struct SlotInfo {
    /// The database this slot is bound to, or an empty string if free.
    pub database: String,
    /// Whether a `SlotGuard` is currently holding this slot's connection.
    pub busy: bool,
    /// Approximate `SystemTime` when this slot was last used.
    pub last_used: SystemTime,
}

// ═══════════════════════════════════════════════════════════════════════════
// Internal slot data
// ═══════════════════════════════════════════════════════════════════════════

struct Slot<C: Connector> {
    /// The live connection, or `None` if the slot is free or the connection is
    /// currently loaned out via a `SlotGuard`.
    conn: Option<C::Conn>,
    /// The database this slot is bound to.  `None` means the slot is free.
    database: Option<String>,
    /// Monotonic timestamp of the last acquisition or release.
    last_used: Instant,
    /// Whether a `SlotGuard` is currently holding this slot's connection.
    busy: bool,
    /// Set by [`SlotManager::disconnect_all`]; the next guard drop will close
    /// the connection instead of returning it.
    disconnect_pending: bool,
    /// Cancel handle for the connection currently bound to this slot.
    /// `Some` iff `database` is `Some` AND the slot has reached the "live
    /// connection" state at least once.  Cleared on close/eviction.
    cancel: Option<C::Cancel>,
}

impl<C: Connector> Slot<C> {
    fn free() -> Self {
        Self {
            conn: None,
            database: None,
            last_used: Instant::now(),
            busy: false,
            disconnect_pending: false,
            cancel: None,
        }
    }
}

// ---------------------------------------------------------------------------
// SlotDecision — returned by the selection logic
// ---------------------------------------------------------------------------

/// The outcome of applying the four acquisition rules against the current
/// slot vector.  The caller must act on this outside the lock.
enum SlotDecision<C: Connector> {
    /// **Rule 1**: an idle connection to the requested database exists.
    /// Use it directly.
    Reuse { idx: usize, conn: C::Conn },

    /// **Rule 2 or 3**: a new connection is needed.
    /// - `idx`: which slot is now reserved (busy, with `database` set).
    /// - `evict_conn`: if `Some`, the old connection must be closed before
    ///   opening the new one.
    NeedsConnect {
        idx: usize,
        evict_conn: Option<C::Conn>,
    },
}

// ═══════════════════════════════════════════════════════════════════════════
// SlotGuard  (RAII)
// ═══════════════════════════════════════════════════════════════════════════

/// RAII guard that holds a slot's connection.
///
/// While the guard lives, the slot is considered **busy** and cannot be evicted
/// or returned to the free pool.
///
/// When the guard is dropped:
/// - The slot is marked idle and `last_used` is updated.
/// - The connection is returned to the slot.
/// - If [`SlotManager::disconnect_all`] was called while the guard was
///   outstanding, the connection is **closed** instead of returned.
pub struct SlotGuard<'a, C: Connector> {
    manager: &'a SlotManager<C>,
    slot_idx: usize,
    conn: Option<C::Conn>,
}

impl<C: Connector> Deref for SlotGuard<'_, C> {
    type Target = C::Conn;

    fn deref(&self) -> &C::Conn {
        self.conn
            .as_ref()
            .expect("SlotGuard always holds a connection")
    }
}

impl<C: Connector> std::ops::DerefMut for SlotGuard<'_, C> {
    fn deref_mut(&mut self) -> &mut C::Conn {
        self.conn
            .as_mut()
            .expect("SlotGuard always holds a connection")
    }
}

impl<C: Connector> AsRef<C::Conn> for SlotGuard<'_, C> {
    fn as_ref(&self) -> &C::Conn {
        self.conn
            .as_ref()
            .expect("SlotGuard always holds a connection")
    }
}

impl<C: Connector> Drop for SlotGuard<'_, C> {
    fn drop(&mut self) {
        // Take ownership of the connection — this is the only chance we get.
        let conn = self
            .conn
            .take()
            .expect("SlotGuard always holds a connection");

        // Lock the slot vector.  Every access inside the lock is synchronous
        // (field assignments), so `std::sync::Mutex` is appropriate.
        let mut slots = match self.manager.slots.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                // Mutex poisoned — best-effort: just leak the connection
                // and continue unwinding.
                drop(poisoned.into_inner());
                return;
            }
        };

        let slot = &mut slots[self.slot_idx];
        slot.busy = false;

        if slot.disconnect_pending {
            // `disconnect_all` was called while we held the guard.
            // Close the connection asynchronously (one-shot cleanup).
            slot.conn = None;
            slot.database = None;
            slot.cancel = None;
            slot.disconnect_pending = false;
            drop(slots);

            tokio::spawn(async move {
                C::close(conn).await;
            });
        } else {
            // Normal path — return the connection to the pool.
            slot.conn = Some(conn);
            slot.last_used = Instant::now();
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// SlotManager
// ═══════════════════════════════════════════════════════════════════════════

/// Manages a fixed-size pool of database connections per server.
///
/// # Slot-acquisition rules (PRD §6)
///
/// When something needs a connection to database *X* on this server:
///
/// 1. **Reuse** — An idle slot already bound to *X* is reused.
/// 2. **Bind free** — A free (unbound) slot is bound to *X* and a connection
///    is opened.
/// 3. **LRU-evict** — An idle slot bound to another database *Y* is evicted
///    (closed) and rebound to *X*.  The **least-recently-used** idle slot
///    (smallest `last_used`) is chosen.
/// 4. **Fail** — If all slots are busy, [`SlotError::AllBusy`] is returned.
///
/// # Principle 1 (no hidden connections)
///
/// This manager never opens a connection on its own.  Only [`acquire`] calls
/// the connector.  No background tasks, keepalives, or pre-fetching exist.
///
/// # `disconnect_all` and busy slots
///
/// [`disconnect_all`](SlotManager::disconnect_all) closes idle connections
/// immediately.  For busy slots, a flag is set so the connection is **closed**
/// (via `tokio::spawn`) when the outstanding [`SlotGuard`] is eventually
/// dropped — it is *not* returned to the pool.  This is a one-shot cleanup;
/// it does not constitute a persistent background task in the sense of
/// AGENTS.md principle 1.
pub struct SlotManager<C: Connector> {
    connector: C,
    slots: Mutex<Vec<Slot<C>>>,
    budget: AtomicUsize,
}

impl<C: Connector> SlotManager<C> {
    /// Create a new manager with the given connector and slot budget.
    ///
    /// All `budget` slots start free (unbound).
    pub fn new(connector: C, budget: usize) -> Self {
        let mut slots = Vec::with_capacity(budget);
        for _ in 0..budget {
            slots.push(Slot::free());
        }
        Self {
            connector,
            slots: Mutex::new(slots),
            budget: AtomicUsize::new(budget),
        }
    }

    /// Acquire a slot bound to `database`.
    ///
    /// Returns a [`SlotGuard`] that holds the connection.  When the guard is
    /// dropped the connection is returned to the slot (or closed if
    /// `disconnect_all` intervened).
    ///
    /// # Cancellation safety
    ///
    /// If the returned future is cancelled or errors after a slot has been
    /// reserved but before the connection is established, the slot is
    /// restored to its previous (free/unbound) state.
    pub async fn acquire(&self, database: &str) -> Result<SlotGuard<'_, C>, SlotError> {
        let db = database.to_string();

        // ── Phase 1: select a slot (under lock) ──────────────────────
        let decision = {
            let mut slots = self.slots.lock().unwrap();

            // Grow the vec if budget was increased since construction.
            let budget = self.budget.load(Ordering::Relaxed);
            while slots.len() < budget {
                slots.push(Slot::free());
            }

            apply_rules(&mut slots, &db, budget)?
        };

        // ── Phase 2: act on the decision (outside the lock) ──────────
        match decision {
            // ── Rule 1: reuse existing connection — no I/O needed ────
            SlotDecision::Reuse { idx, conn } => Ok(SlotGuard {
                manager: self,
                slot_idx: idx,
                conn: Some(conn),
            }),

            // ── Rules 2 & 3: close old conn (if any), then connect ──
            SlotDecision::NeedsConnect { idx, evict_conn } => {
                // Close the evicted connection (best-effort).
                if let Some(old_conn) = evict_conn {
                    C::close(old_conn).await;
                }

                // Use a drop-guard for cancellation safety: if the future
                // is cancelled before connect completes, the slot is restored.
                let recovery = Recovery {
                    slots: &self.slots,
                    idx,
                    recovered: std::cell::Cell::new(false),
                };

                let (new_conn, new_cancel) = self.connector.connect(&db).await.map_err(|e| {
                    // The drop of `recovery` will restore the slot.
                    SlotError::Connect(e)
                })?;

                // Slot's cancel is set here; it persists across busy/idle transitions
                // until eviction or disconnect_all.
                {
                    let mut slots = self.slots.lock().unwrap();
                    slots[idx].cancel = Some(new_cancel);
                }

                recovery.recovered.set(true);
                // recovery drops here without restoring.

                Ok(SlotGuard {
                    manager: self,
                    slot_idx: idx,
                    conn: Some(new_conn),
                })
            }
        }
    }

    /// Return a snapshot of the current slot state.
    pub fn state(&self) -> SlotState {
        let slots = self.slots.lock().unwrap();
        let budget = self.budget.load(Ordering::Relaxed);
        let now = SystemTime::now();
        SlotState {
            budget,
            slots: slots
                .iter()
                .map(|s| {
                    let elapsed = s.last_used.elapsed();
                    SlotInfo {
                        database: s.database.clone().unwrap_or_default(),
                        busy: s.busy,
                        last_used: now.checked_sub(elapsed).unwrap_or(SystemTime::UNIX_EPOCH),
                    }
                })
                .collect(),
        }
    }

    /// Close all connections and mark all slots as free.
    ///
    /// Idle connections are closed immediately.  Busy slots are flagged so
    /// their connection is closed when the outstanding guard is dropped.
    ///
    /// # Cancel handles on busy slots
    ///
    /// Busy slots keep their cancel handle alive during the closing window.
    /// If a user cancels a query mid-disconnect, the cancel may race with the
    /// conn closing — both are safe; whichever finishes first wins.
    /// The guard's drop clears the cancel.
    pub async fn disconnect_all(&self) {
        // Collect all idle connections to close outside the lock.
        let mut to_close: Vec<C::Conn> = Vec::new();

        {
            let mut slots = self.slots.lock().unwrap();
            for slot in slots.iter_mut() {
                if slot.busy {
                    // Will be closed when the guard drops.
                    slot.disconnect_pending = true;
                } else if let Some(conn) = slot.conn.take() {
                    // Close below, after dropping the lock.
                    slot.database = None;
                    slot.cancel = None;
                    to_close.push(conn);
                }
            }
        } // Lock released.

        for conn in to_close {
            C::close(conn).await;
        }
    }

    /// Set the slot budget.
    ///
    /// In M1, only increases are allowed.  Decreases return
    /// [`SlotError::CannotShrink`].
    pub fn set_budget(&self, new_budget: usize) -> Result<(), SlotError> {
        let mut slots = self.slots.lock().unwrap();
        let old = slots.len();
        if new_budget < old {
            return Err(SlotError::CannotShrink);
        }
        slots.resize_with(new_budget, Slot::free);
        self.budget.store(new_budget, Ordering::Relaxed);
        Ok(())
    }

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

// ── Selection logic (synchronous, run inside the lock) ─────────────────

/// Apply acquisition rules 1–4 and return the decision.
///
/// The slot is already updated (busy flag set, database assigned, connection
/// extracted) before returning, so the caller only has to perform I/O.
fn apply_rules<C: Connector>(
    slots: &mut [Slot<C>],
    database: &str,
    budget: usize,
) -> Result<SlotDecision<C>, SlotError> {
    // --- Rule 1: idle slot already bound to this database ---
    for (i, slot) in slots.iter_mut().enumerate() {
        if !slot.busy && slot.database.as_deref() == Some(database) {
            let conn = slot
                .conn
                .take()
                .expect("idle bound slot must hold a connection");
            slot.busy = true;
            slot.last_used = Instant::now();
            return Ok(SlotDecision::Reuse { idx: i, conn });
        }
    }

    // --- Rule 2: free (unbound) slot ---
    for (i, slot) in slots.iter_mut().enumerate() {
        if !slot.busy && slot.conn.is_none() {
            debug_assert!(slot.database.is_none(), "no-conn slot must be unbound");
            slot.busy = true;
            slot.database = Some(database.to_string());
            slot.last_used = Instant::now();
            return Ok(SlotDecision::NeedsConnect {
                idx: i,
                evict_conn: None,
            });
        }
    }

    // --- Rule 3: LRU-evict an idle slot bound to another database ---
    let mut best_idx = None;
    let mut best_last_used = Instant::now(); // placeholder, overwritten
    for (i, slot) in slots.iter().enumerate() {
        if !slot.busy
            && slot.conn.is_some()
            && slot.database.as_deref() != Some(database)
            && (best_idx.is_none() || slot.last_used < best_last_used)
        {
            best_idx = Some(i);
            best_last_used = slot.last_used;
        }
    }

    if let Some(i) = best_idx {
        let slot = &mut slots[i];
        let evict_conn = slot.conn.take().expect("checked above");
        let _old_db = slot.database.take(); // no longer bound to the old database
        slot.cancel = None; // invalid once the conn is closed
        slot.busy = true;
        slot.database = Some(database.to_string());
        slot.last_used = Instant::now();
        return Ok(SlotDecision::NeedsConnect {
            idx: i,
            evict_conn: Some(evict_conn),
        });
    }

    // --- Rule 4: all slots busy ---
    Err(SlotError::AllBusy(budget))
}

// ── Drop-guard for cancellation safety ─────────────────────────────────

/// Restores a reserved slot to free/unbound if the caller did not confirm
/// (via `recovered`) before this guard is dropped.
///
/// This handles two scenarios:
/// - The `acquire` future is **cancelled** (e.g. the caller dropped the
///   future) after the slot was reserved but before `connect` completed.
/// - `connect` returned an error.
struct Recovery<'a, C: Connector> {
    slots: &'a Mutex<Vec<Slot<C>>>,
    idx: usize,
    recovered: std::cell::Cell<bool>,
}

impl<C: Connector> Drop for Recovery<'_, C> {
    fn drop(&mut self) {
        if !self.recovered.get() {
            // The caller did not confirm — restore the slot.
            if let Ok(mut guard) = self.slots.lock() {
                let slot = &mut guard[self.idx];
                slot.conn = None;
                slot.database = None;
                slot.cancel = None;
                slot.busy = false;
                slot.disconnect_pending = false;
            }
            // If the lock is poisoned, there's nothing we can do.
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    // ── FakeConnector ────────────────────────────────────────────────

    /// A minimal cancel handle used by [`FakeConnector`].  Carries a
    /// counter so tests can assert cancel handles were retrievable.
    #[derive(Clone, Default)]
    struct FakeCancel {
        #[allow(dead_code)]
        cancel_count: Arc<AtomicUsize>,
    }

    /// A connection value produced by [`FakeConnector`].
    ///
    /// Carries a reference to the shared close counter so that
    /// `Connector::close` can increment it (that method is an associated
    /// function, not a method, so it has no access to the connector itself).
    struct FakeConn {
        _id: u32,
        close_counter: Arc<AtomicUsize>,
    }

    struct FakeConnector {
        next_id: AtomicUsize,
        connect_counter: Arc<AtomicUsize>,
        close_counter: Arc<AtomicUsize>,
    }

    impl FakeConnector {
        fn new() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
            let connect_counter = Arc::new(AtomicUsize::new(0));
            let close_counter = Arc::new(AtomicUsize::new(0));
            let connector = Self {
                next_id: AtomicUsize::new(0),
                connect_counter: connect_counter.clone(),
                close_counter: close_counter.clone(),
            };
            (connector, connect_counter, close_counter)
        }
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
            // Allow close to settle.
            tokio::task::yield_now().await;
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────

    /// A small sleep that yields control so background spawns complete.
    async fn tick() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // ── Rule 1: reuse idle ───────────────────────────────────────────

    #[tokio::test]
    async fn rule1_reuse_idle_slot() {
        let (conn, connects, closes) = FakeConnector::new();
        let mgr = SlotManager::new(conn, 2);

        let g1 = mgr.acquire("A").await.unwrap();
        drop(g1); // slot 0 now idle, bound to A

        let g2 = mgr.acquire("A").await.unwrap();
        drop(g2);

        // Only one connect call — second acquire reused the idle slot.
        assert_eq!(connects.load(Ordering::SeqCst), 1);
        assert_eq!(closes.load(Ordering::SeqCst), 0);
    }

    // ── Rule 2: bind free slot ───────────────────────────────────────

    #[tokio::test]
    async fn rule2_bind_free_slot() {
        let (conn, connects, closes) = FakeConnector::new();
        let mgr = SlotManager::new(conn, 2);

        let g1 = mgr.acquire("A").await.unwrap();
        let g2 = mgr.acquire("B").await.unwrap();
        drop((g1, g2));

        // budget=2 → two separate opens with no eviction.
        assert_eq!(connects.load(Ordering::SeqCst), 2);
        assert_eq!(closes.load(Ordering::SeqCst), 0);
    }

    // ── Rule 3: LRU eviction ─────────────────────────────────────────

    #[tokio::test]
    async fn rule3_lru_evicts_least_recently_used() {
        let (conn, connects, closes) = FakeConnector::new();
        let mgr = SlotManager::new(conn, 2);

        // 1. Acquire A, drop → slot 0 idle with A.
        let g = mgr.acquire("A").await.unwrap();
        drop(g);

        // 2. Acquire B, drop → slot 1 idle with B (later last_used).
        tokio::time::sleep(Duration::from_millis(10)).await;
        let g = mgr.acquire("B").await.unwrap();
        drop(g);

        // 3. Touch A again (acquire + drop) → slot 0 last_used now newer.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let g = mgr.acquire("A").await.unwrap();
        drop(g);

        // 4. Acquire C → must evict.  LRU among idle = B (slot 1).
        tokio::time::sleep(Duration::from_millis(10)).await;
        let g = mgr.acquire("C").await.unwrap();
        drop(g);

        // Three connects (A, B, C).  One close (B).
        assert_eq!(connects.load(Ordering::SeqCst), 3);
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    // ── Rule 4: all busy ─────────────────────────────────────────────

    #[tokio::test]
    async fn rule4_all_busy() {
        let (conn, _connects, _closes) = FakeConnector::new();
        let mgr = SlotManager::new(conn, 1);

        // Hold the only slot.
        let guard = mgr.acquire("A").await.unwrap();
        let result = mgr.acquire("B").await;

        drop(guard);

        assert!(
            matches!(result, Err(SlotError::AllBusy(1))),
            "expected AllBusy(1)"
        );
    }

    // ── Concurrency: two tasks, same DB, budget=2 ────────────────────

    #[tokio::test]
    async fn concurrency_two_tasks_same_db() {
        let (conn, connects, closes) = FakeConnector::new();
        let mgr = Arc::new(SlotManager::new(conn, 2));

        let mgr1 = mgr.clone();
        let t1 = tokio::spawn(async move {
            let guard = mgr1.acquire("A").await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(guard);
        });

        let mgr2 = mgr.clone();
        let t2 = tokio::spawn(async move {
            // Slight delay so that t1 reserves its slot first (both will
            // have slot 0 busy and slot 1 free → two connects to "A").
            tokio::time::sleep(Duration::from_millis(5)).await;
            let guard = mgr2.acquire("A").await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(guard);
        });

        let _ = tokio::join!(t1, t2);

        // Both tasks opened their own connection to "A" (budget=2).
        assert_eq!(connects.load(Ordering::SeqCst), 2);
        assert_eq!(closes.load(Ordering::SeqCst), 0);
    }

    // ── disconnect_all ───────────────────────────────────────────────

    #[tokio::test]
    async fn disconnect_all_closes_idle_and_pending() {
        let (conn, connects, closes) = FakeConnector::new();
        let mgr = SlotManager::new(conn, 2);

        // Fill both slots, then release.
        let g1 = mgr.acquire("A").await.unwrap();
        let g2 = mgr.acquire("B").await.unwrap();
        drop(g1);
        drop(g2);

        // disconnect_all: both idle → closed synchronously.
        mgr.disconnect_all().await;
        tick().await;

        assert_eq!(closes.load(Ordering::SeqCst), 2, "both should be closed");

        // Next acquire opens fresh.
        let g = mgr.acquire("A").await.unwrap();
        drop(g);
        assert_eq!(connects.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn disconnect_all_with_busy_slot() {
        let (conn, _connects, closes) = FakeConnector::new();
        let mgr = SlotManager::new(conn, 2);

        // Fill both slots.  Keep one guard alive.
        let g1 = mgr.acquire("A").await.unwrap(); // held
        let g2 = mgr.acquire("B").await.unwrap();
        drop(g2); // idle

        // disconnect_all: idle (B) closed immediately; busy (A) flagged.
        mgr.disconnect_all().await;
        tick().await;

        // Only B was closed so far.
        assert_eq!(closes.load(Ordering::SeqCst), 1);

        // Drop the busy guard → A's connection is closed via spawn.
        drop(g1);
        tick().await;

        assert_eq!(
            closes.load(Ordering::SeqCst),
            2,
            "A should be closed on drop"
        );
    }

    // ── set_budget ───────────────────────────────────────────────────

    #[tokio::test]
    async fn set_budget_increase_works() {
        let (conn, _connects, _closes) = FakeConnector::new();
        let mgr = SlotManager::new(conn, 2);

        assert_eq!(mgr.state().budget, 2);

        mgr.set_budget(3).unwrap();
        assert_eq!(mgr.state().budget, 3);

        // Decreasing must fail.
        let err = mgr.set_budget(1).unwrap_err();
        assert!(
            matches!(err, SlotError::CannotShrink),
            "expected CannotShrink, got {err}"
        );

        // Budget unchanged after failed shrink.
        assert_eq!(mgr.state().budget, 3);
    }

    // ── state() snapshot ─────────────────────────────────────────────

    #[tokio::test]
    async fn state_reflects_current_slots() {
        let (conn, _connects, _closes) = FakeConnector::new();
        let mgr = SlotManager::new(conn, 2);

        // Initially all slots are free (database string is empty).
        let state = mgr.state();
        assert_eq!(state.budget, 2);
        assert!(
            state.slots.iter().all(|s| !s.busy && s.database.is_empty()),
            "all slots should be free initially: got {state:?}"
        );

        let g = mgr.acquire("my-db").await.unwrap();
        let state = mgr.state();
        // 2 total slots (budget=2), 1 bound (busy), 1 free
        assert_eq!(state.slots.len(), 2);
        let busy_slots: Vec<_> = state.slots.iter().filter(|s| s.busy).collect();
        assert_eq!(busy_slots.len(), 1);
        assert_eq!(busy_slots[0].database, "my-db");

        drop(g);

        let state = mgr.state();
        assert!(
            state.slots.iter().all(|s| !s.busy),
            "all slots should be idle after dropping guard"
        );
        let bound: Vec<_> = state
            .slots
            .iter()
            .filter(|s| !s.database.is_empty())
            .collect();
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].database, "my-db");
    }

    // ── Cancellation safety ──────────────────────────────────────────

    /// A connector that never completes `connect` — used to test that a
    /// cancelled acquire restores the slot.
    struct HangingConnector;

    #[async_trait]
    impl Connector for HangingConnector {
        type Conn = ();
        type Cancel = ();

        async fn connect(
            &self,
            _database: &str,
        ) -> Result<(Self::Conn, Self::Cancel), ConnectorError> {
            std::future::pending().await
        }

        async fn close(conn: Self::Conn) {
            let _ = conn;
        }
    }

    #[tokio::test]
    async fn cancelled_acquire_restores_slot() {
        // Use tokio::time::timeout to cancel the acquire.  When the timeout
        // fires, the inner future (acquire) is dropped, which triggers
        // Recovery::drop to restore the slot.
        let mgr = SlotManager::new(HangingConnector, 1);

        let result = tokio::time::timeout(Duration::from_millis(10), mgr.acquire("A")).await;

        assert!(result.is_err(), "timeout should have elapsed");

        // The acquire future was dropped via timeout — slot should be free.
        let state = mgr.state();
        assert!(
            state.slots.iter().all(|s| !s.busy && s.database.is_empty()),
            "cancelled acquire should restore slot to free: got {state:?}"
        );
    }

    // ── busy_cancel_handles ───────────────────────────────────────────

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
}
