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

// ── Query results (mirrors query::RunResult / ChunkResult) ──

export type RunResult = {
  result_id: string;
  columns: ColumnMeta[];
  first_chunk: unknown[][];
  has_more: boolean;
  row_count_so_far: number;
  duration_ms_so_far: number;
};

export type ChunkResult = {
  rows: unknown[][];
  has_more: boolean;
  row_count_so_far: number;
  duration_ms_so_far: number;
};

export type ColumnMeta = {
  name: string;
  type_name: string;
};

// ── Introspection types (mirrors introspect::*) ──

export type DatabaseInfo = { name: string };

export type RelationKind =
  | "table"
  | "view"
  | "matview"
  | "partitioned_table";

export type ColumnInfo = {
  name: string;
  type_name: string;
  not_null: boolean;
  position: number;
};

export type RelationInfo = {
  name: string;
  kind: RelationKind;
  columns: ColumnInfo[];
};

export type FunctionKind =
  | "function"
  | "procedure"
  | "aggregate"
  | "window";

export type FunctionInfo = { name: string; kind: FunctionKind };

export type SchemaInfoPayload = {
  name: string;
  relations: RelationInfo[];
  functions: FunctionInfo[];
};

export type SchemaPayload = {
  v: number;
  schemas: SchemaInfoPayload[];
  search_path: string[];
};

// ── Completion analysis (mirrors parse::*) ──

export type CompletionKind =
  | "none"
  | "from_item"
  | "qualified_relation"
  | "qualified_column"
  | "unqualified";

export type ScopeTable = {
  schema: string | null;
  name: string;
  alias: string | null;
};

export type CompletionContext = {
  kind: CompletionKind;
  qualifier: string | null;
  prefix: string;
  from_offset: number;
  scope_tables: ScopeTable[];
};

// ── Cancellation (mirrors commands::CancelOutcome) ──

export type CancelOutcome = {
  cancelled: number;
  errors: string[];
};

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

// ── Error type (mirrors commands::CommandError serde tagging) ──

export type CommandError = {
  kind:
    | "UnknownConnection"
    | "NotConnected"
    | "Slot"
    | "Pg"
    | "Store"
    | "Introspect"
    | "Saved";
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

  runQuery: (
    serverId: number,
    database: string,
    sql: string,
    chunkSize: number | null = null,
  ) =>
    invoke<RunResult>("run_query", { serverId, database, sql, chunkSize }),

  fetchMore: (resultId: string, chunkSize: number | null = null) =>
    invoke<ChunkResult>("fetch_more", { resultId, chunkSize }),

  closeResult: (resultId: string) =>
    invoke<void>("close_result", { resultId }),

  getSlotState: (serverId: number) =>
    invoke<SlotState | null>("get_slot_state", { serverId }),

  listDatabases: (serverId: number) =>
    invoke<DatabaseInfo[]>("list_databases", { serverId }),

  listSchemas: (serverId: number, database: string) =>
    invoke<string[]>("list_schemas", { serverId, database }),

  listRelations: (serverId: number, database: string, schema: string) =>
    invoke<RelationInfo[]>("list_relations", { serverId, database, schema }),

  listFunctions: (serverId: number, database: string, schema: string) =>
    invoke<FunctionInfo[]>("list_functions", { serverId, database, schema }),

  getSchemaPayload: (serverId: number, database: string) =>
    invoke<SchemaPayload>("get_schema_payload", { serverId, database }),

  refreshSchemaCache: (serverId: number, database: string) =>
    invoke<void>("refresh_schema_cache", { serverId, database }),

  cancelQuery: (serverId: number, database: string | null = null) =>
    invoke<CancelOutcome>("cancel_query", { serverId, database }),

  analyzeCompletion: (sql: string, cursor: number) =>
    invoke<CompletionContext>("analyze_completion", { sql, cursor }),

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
};
