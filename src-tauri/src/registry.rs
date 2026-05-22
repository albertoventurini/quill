//! Per-process registry of live server connections.
//!
//! One `ServerHandle` per *connected* saved server (keyed by the SQLite
//! `connections.id`).  The registry stays empty until M1.5's
//! `connect_server` command inserts an entry.

use std::sync::Arc;

use dashmap::DashMap;

use crate::introspect::SchemaPayload;
use crate::pg::PgConnector;
use crate::slots::SlotManager;

/// Live handle for one connected server.
///
/// Both fields are `Arc`-wrapped so the handle can be cloned cheaply out of
/// the registry's `DashMap` shard lock before any `.await`.
///
/// `schema_cache` is the session-scoped in-memory schema cache.  It is
/// created empty when the user connects and discarded when they disconnect,
/// so it can never be stale across restarts.  Within a session the first
/// expand of a database populates the entry; subsequent expands of the same
/// database (or of any of its schemas) return the cached payload at zero
/// slot cost.
#[derive(Clone)]
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

/// Registered as Tauri managed state.  Empty at startup.
#[derive(Default)]
pub struct ServerRegistry {
    pub by_id: DashMap<i64, ServerHandle>,
}
