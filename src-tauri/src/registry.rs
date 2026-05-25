//! Per-process registry of live server connections.
//!
//! One `ServerHandle` per *connected* saved server (keyed by the SQLite
//! `connections.id`).  The registry stays empty until M1.5's
//! `connect_server` command inserts an entry.

use std::time::SystemTime;

use std::sync::Arc;

use dashmap::DashMap;

use crate::introspect::SchemaPayload;
use crate::pg::PgConnector;
use crate::slots::SlotManager;

#[derive(Clone)]
pub struct ServerHandle {
    pub slot_manager: Arc<SlotManager<PgConnector>>,
    pub schema_cache: Arc<DashMap<String, SchemaPayload>>,
    pub credential_expiry: Option<SystemTime>,
}

impl ServerHandle {
    pub fn new(connector: PgConnector, budget: usize) -> Self {
        Self {
            slot_manager: Arc::new(SlotManager::new(connector, budget)),
            schema_cache: Arc::new(DashMap::new()),
            credential_expiry: None,
        }
    }
}

/// Registered as Tauri managed state.  Empty at startup.
#[derive(Default)]
pub struct ServerRegistry {
    pub by_id: DashMap<i64, ServerHandle>,
}
