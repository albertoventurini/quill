import { invoke } from "@tauri-apps/api/core";

// ── Connection types (mirrors store::Connection / store::NewConnection) ──

export type Connection = {
  id: number;
  name: string;
  host: string;
  port: number;
  default_db: string;
  username: string;
  ssl_mode: string;
  slot_budget: number;
  password_ref: string | null;
  created_at: string;
};

/** Fields for creating a new connection. `password_ref` must be `null` in
 *  M1 — the OS keychain lands in M6. */
export type NewConnection = {
  name: string;
  host: string;
  port: number;
  default_db: string;
  username: string;
  ssl_mode: string;
  slot_budget: number;
  password_ref: null;
};

// ── Slot types (mirrors slots::SlotState / slots::SlotInfo) ──

/** SystemTime serializes as a struct with two number fields. */
export type SlotInfo = {
  database: string;
  busy: boolean;
  last_used: { secs_since_epoch: number; nanos_since_epoch: number };
};

export type SlotState = {
  budget: number;
  slots: SlotInfo[];
};

// ── Query result types (mirrors commands::QueryResult / ColumnMeta) ──

/** `rows` holds `serde_json::Value` cells — null, bool, number, string,
 *  array, or object. */
export type QueryResult = {
  columns: ColumnMeta[];
  rows: unknown[][];
  row_count: number;
  duration_ms: number;
};

export type ColumnMeta = {
  name: string;
  type_name: string;
};

// ── Error type (mirrors commands::CommandError serde tagging) ──

export type CommandError = {
  kind: "UnknownConnection" | "NotConnected" | "Slot" | "Pg" | "Store";
  message: string;
};

// ── Typed API ──

/** Every method wraps `invoke` with the correct parameter names and
 *  return type.  On error, the invoke call rejects with a `CommandError`
 *  object; callers must catch and inspect `(e as CommandError).kind`. */
export const api = {
  listConnections: () =>
    invoke<Connection[]>("list_connections"),

  saveConnection: (newConn: NewConnection) =>
    invoke<Connection>("save_connection", { new: newConn }),

  deleteConnection: (id: number) =>
    invoke<void>("delete_connection", { id }),

  connectServer: (id: number, password: string) =>
    invoke<SlotState>("connect_server", { id, password }),

  disconnectServer: (id: number) =>
    invoke<void>("disconnect_server", { id }),

  runQuery: (serverId: number, database: string, sql: string) =>
    invoke<QueryResult>("run_query", { serverId, database, sql }),

  getSlotState: (serverId: number) =>
    invoke<SlotState | null>("get_slot_state", { serverId }),
};
