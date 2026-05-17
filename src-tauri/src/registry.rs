//! Per-process registry of live server connections.
//!
//! One `ServerHandle` per *connected* saved server (keyed by the SQLite
//! `connections.id`).  The registry stays empty until M1.5's
//! `connect_server` command inserts an entry.

use std::sync::Arc;

use dashmap::DashMap;

use crate::pg::PgConnector;
use crate::slots::SlotManager;

/// Live handle for one connected server.
///
/// The `SlotManager` is wrapped in `Arc` so command handlers can clone it
/// out of the map and use it without holding a `DashMap` shard lock across
/// an `.await`.
#[derive(Clone)]
pub struct ServerHandle {
    pub slot_manager: Arc<SlotManager<PgConnector>>,
}

impl ServerHandle {
    pub fn new(connector: PgConnector, budget: usize) -> Self {
        Self {
            slot_manager: Arc::new(SlotManager::new(connector, budget)),
        }
    }
}

/// Registered as Tauri managed state.  Empty at startup.
#[derive(Default)]
pub struct ServerRegistry {
    pub by_id: DashMap<i64, ServerHandle>,
}
