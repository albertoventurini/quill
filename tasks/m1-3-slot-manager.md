# M1.3 — Slot manager (headless, unit-tested)

## Goal
Build the connection-slot manager: a pure-Rust module that enforces "no more than *N* live connections per server, opened only when an explicit user action demands one." This is the heart of Quill's reason to exist.

## Context to read first
- `PRD.md` §6 — slot model and acquisition rules. **Read in full; the rules are precise.**
- `AGENTS.md` — design principles 1 (no hidden connections) and 2 (pool is a budget).

## What a slot is
A live connection bound to **one database at a time**, on a specific server. Each saved server has *N* slots (default 2, user-configurable).

## Slot acquisition rules (verbatim from PRD §6)
When something needs to talk to database `X` on server `S`:
1. A slot on `S` already bound to `X` and **idle** → reuse it.
2. A **free** (unbound) slot on `S` exists → bind it to `X` (connect).
3. An **idle** slot on `S` is bound to some other database `Y` → evict `Y` (close), rebind to `X`. **LRU** across idle slots.
4. All slots on `S` are **busy** → fail fast with `SlotError::AllBusy` for M1 (queueing comes later).

## Deliverables

### 1. Module
`src-tauri/src/slots/mod.rs`. Public API shape:
```rust
#[async_trait::async_trait]
pub trait Connector: Send + Sync + 'static {
    type Conn: Send + 'static;
    async fn connect(&self, database: &str) -> Result<Self::Conn, ConnectorError>;
    async fn close(conn: Self::Conn);   // best-effort; may swallow errors
}

pub struct SlotManager<C: Connector> { /* … */ }

impl<C: Connector> SlotManager<C> {
    pub fn new(connector: C, budget: usize) -> Self;
    pub async fn acquire(&self, database: &str) -> Result<SlotGuard<'_, C>, SlotError>;
    pub fn state(&self) -> SlotState;
    pub async fn disconnect_all(&self);
    pub fn set_budget(&self, budget: usize); // M1: increase only; decrease returns SlotError::CannotShrink
}

pub struct SlotGuard<'a, C: Connector> { /* Deref<Target = C::Conn> */ }
// Dropping the guard marks the slot idle and updates last_used.

#[derive(Debug, Clone, Serialize)]
pub struct SlotState { pub budget: usize, pub slots: Vec<SlotInfo> }

#[derive(Debug, Clone, Serialize)]
pub struct SlotInfo { pub database: String, pub busy: bool, pub last_used: SystemTime }
```

### 2. Internal data structure
- `tokio::sync::Mutex<Vec<Slot>>` (or `parking_lot` if everything inside the lock is sync).
- Each `Slot`: `Option<C::Conn>`, `Option<String> database`, `Instant last_used`, `bool busy`.

### 3. Behavior details
- **LRU eviction (rule 3):** among idle slots bound to a different database, evict the one with the smallest `last_used`. Call `Connector::close` on its connection before opening the new one.
- **Cancellation safety:** if `connect` fails, the slot returns to `free`, not "bound-but-broken".
- **`disconnect_all`:** closes all idle slots immediately; for busy slots, the simplest behavior is to wait for their guards to drop, then close. Document the choice in a doc-comment.
- **`set_budget`:** increase only in M1; shrinking returns `SlotError::CannotShrink` (deferred).
- **No background tasks.** No timers, keepalives, or pings — see AGENTS.md principle 1.

### 4. Errors
```rust
#[derive(thiserror::Error, Debug)]
pub enum SlotError {
    #[error("no slot available; all {0} slots are busy")]
    AllBusy(usize),
    #[error("cannot shrink slot budget (M1 limitation)")]
    CannotShrink,
    #[error("connect failed: {0}")]
    Connect(#[from] ConnectorError),
}
```

## Tests
`#[cfg(test)] mod tests` in `slots/mod.rs`. Implement a `FakeConnector` that yields integer "connections" and tracks open/close counts per database. Cover:

- **Rule 1**: acquire DB-A twice serially → one open, second reuses.
- **Rule 2**: budget=2, acquire A then B serially → two opens, two slots populated.
- **Rule 3 (LRU)**: budget=2, acquire A, drop, acquire B, drop, touch A again (acquire+drop), acquire C → B is evicted, not A. Exactly one `close` call, against B.
- **Rule 4**: budget=1, acquire A (hold guard), try to acquire B → `SlotError::AllBusy(1)`.
- **Concurrency**: budget=2, two async tasks acquire same DB → second waits or opens a second slot per the rules; no second `connect` for the same DB if the first is still holding the slot.
- **`disconnect_all`**: opens A and B, then `disconnect_all` → both `close`d, next `acquire` opens fresh.
- **`set_budget`**: increasing works; decreasing returns `CannotShrink`.

## Dependencies (add to `Cargo.toml`)
- `async-trait = "0.1"`
- (`tokio`, `serde`, `thiserror` already from M1.2)

## Acceptance criteria
- [ ] `./test.sh` passes; all acquisition rules covered.
- [ ] No real Postgres dependency in this module — only `FakeConnector` in tests.
- [ ] Public API matches the shape above with doc-comments explaining *why* (per AGENTS.md).
- [ ] No background tasks spawned anywhere (principle 1).

## Out of scope
- Real Postgres connector — M1.4.
- Queueing on `AllBusy` — later milestone.
- Decreasing the budget — later.
- Cancellation handle plumbing through guards — M3 (the cancellation milestone).
