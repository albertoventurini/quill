# M2.4 — Tree UI for databases, schemas, relations, and functions

## Goal

**Before:** The left pane is the M1.6 flat list — each saved connection is a row with a slot indicator, and selecting a row reveals a password input + Connect button. The right pane has the textarea + DB input + Run + `<pre>` result. The Tauri bridge `src/lib/tauri.ts` exposes only the seven M1.5 commands. None of the M2.3 introspection commands are reachable from the UI.

**After:** The left pane is a recursive lazy-loading tree:

```
▸ local              [0/2]
   ▾ postgres
      ▾ public
         ▾ Tables (3)
            users
            orders
            products
         ▾ Views (1)
            order_summary
         ▾ Functions (2)
            ƒ uuid_generate_v4
            ⌖ refresh_summary
   ▾ analytics
      ▸ metrics
      ▸ ingest
```

(emoji-style icons are *not* used — kind is shown via plain-text tags like `[T]`/`[V]`/`[F]` to honour the "no emojis unless asked" rule; the layout above is illustrative.)

Each level lazy-fetches: expanding a connected server lists databases; expanding a database lists schemas (triggering a cache miss → full introspection on first expand for that DB); expanding a schema reveals four group folders (Tables / Views / Materialized Views / Functions). Loading states render inline as `...`; errors render inline as `(error: <message>)`. A right-click context menu offers Connect / Disconnect / Refresh schema / Copy name as relevant per node kind. The slot indicator on each server still reads `[busy/budget]`. The right pane keeps the textarea + Run + `<pre>` result from M1.6, but its `serverId`/`database` are now driven by tree selection: clicking a database node sets the right pane's target to `(server, db)`; clicking a deeper node inherits the database. Connect still prompts for a password — but the password input lives in a small modal that opens when the user invokes Connect from the context menu, not as an always-visible field.

`src/lib/tauri.ts` is extended with the five new method bindings and matching types. The tree is implemented in a new `src/lib/Tree.svelte` recursive component. `src/routes/+page.svelte` is rewritten — the left pane is the tree + a "+ Add Connection" button + the existing Add modal; the right pane is the SQL form. No CSS framework is added.

## Current state

Every file below already exists and is reproduced (in full or in relevant excerpts). Read them before writing anything.

### `src-tauri/src/commands/mod.rs` — relevant command signatures (post-M2.3)

Twelve `#[tauri::command]` functions. The five new ones from M2.3:

| Command | invoke args (camelCase) | Returns |
|---|---|---|
| `list_databases` | `{ serverId }` | `DatabaseInfo[]` |
| `list_schemas` | `{ serverId, database }` | `string[]` |
| `list_relations` | `{ serverId, database, schema }` | `RelationInfo[]` |
| `list_functions` | `{ serverId, database, schema }` | `FunctionInfo[]` |
| `refresh_schema_cache` | `{ serverId, database }` | `SchemaPayload` |

`CommandError` gained two new variants in M2.3:
```rust
pub enum CommandError {
    UnknownConnection(String),
    NotConnected(String),
    Slot(String),
    Pg(String),
    Store(String),
    Introspect(String),     // M2.3
    UnknownDatabase(String),// M2.3
}
```

### `src-tauri/src/introspect/mod.rs` — wire shapes (post-M2.2)

```rust
pub struct DatabaseInfo { pub name: String }
pub struct RelationInfo { pub name: String, pub kind: RelationKind }
pub struct FunctionInfo { pub name: String, pub kind: FunctionKind }
pub struct SchemaPayload { pub v: u32, pub schemas: Vec<SchemaInfo> }
pub struct SchemaInfo { pub name: String, pub relations: Vec<RelationInfo>, pub functions: Vec<FunctionInfo> }

#[serde(rename_all = "snake_case")]
pub enum RelationKind { Table, View, Matview, PartitionedTable }  // -> "table"|"view"|"matview"|"partitioned_table"

#[serde(rename_all = "snake_case")]
pub enum FunctionKind { Function, Procedure, Aggregate, Window }   // -> "function"|"procedure"|"aggregate"|"window"
```

### `src/lib/tauri.ts` — current export surface

```ts
import { invoke } from "@tauri-apps/api/core";

export type Connection = { id; name; host; port; default_db; username; ssl_mode; slot_budget; password_ref; created_at };
export type NewConnection = { name; host; port; default_db; username; ssl_mode; slot_budget; password_ref: null };
export type SlotInfo = { database; busy; last_used };
export type SlotState = { budget; slots };
export type QueryResult = { columns; rows; row_count; duration_ms };
export type ColumnMeta = { name; type_name };
export type CommandError = { kind: "UnknownConnection" | "NotConnected" | "Slot" | "Pg" | "Store"; message: string };

export const api = {
  listConnections: () => invoke<Connection[]>("list_connections"),
  saveConnection: (n) => invoke<Connection>("save_connection", { new: n }),
  deleteConnection: (id) => invoke<void>("delete_connection", { id }),
  connectServer: (id, password) => invoke<SlotState>("connect_server", { id, password }),
  disconnectServer: (id) => invoke<void>("disconnect_server", { id }),
  runQuery: (serverId, database, sql) => invoke<QueryResult>("run_query", { serverId, database, sql }),
  getSlotState: (serverId) => invoke<SlotState | null>("get_slot_state", { serverId }),
};
```

`CommandError.kind` will gain `"Introspect"` and `"UnknownDatabase"` in deliverable 1.

### `src/routes/+page.svelte` — current shape (M1.6)

A two-pane shell with:
- Left pane: `<aside>` containing connection list, "+ Add Connection", and (when a connection is selected) the password input + Connect/Disconnect.
- Right pane: `<main>` with the SQL textarea, DB input, Run button, and `<pre>` result.
- Add-connection `<dialog>` modal.

This file is rewritten by M2.4. The right pane's structure (DB input + textarea + Run + result) is preserved verbatim — only the left pane and the supporting state model change. The Add modal is unchanged.

### `src/app.html`

`<title>Quill</title>` (set in M1.6). Not changed.

### `src/routes/+layout.ts`

```ts
export const ssr = false;
```

Not changed. `$effect` runs in the browser; no `onMount` import needed.

### `package.json`

No new dependencies. No tree-component library, no icon library, no CSS framework — everything is hand-rolled to match the AGENTS.md "no heavy component framework" guidance.

## Design choices baked into this spec

- **Tree state lives on the tree nodes themselves** (`expanded`, `loading`, `error`, `children`), wrapped in Svelte 5 `$state`. Mutations on plain objects/arrays inside `$state` *are* reactive in Svelte 5 (the runtime wraps them in Proxies), so we use direct mutation instead of the immutable-replacement pattern from M1.6 — the tree is too deep for that idiom to scale. **This is a deliberate deviation from M1.6 and worth a doc-comment in the file.**
- **Schema expansion fetches relations and functions in parallel** (`Promise.all([api.listRelations, api.listFunctions])`). Both calls hit the same per-DB cache; the first triggers introspection on cache miss, the second always hits warm cache. This keeps the slot-indicator bump to one acquire per fresh DB.
- **Group folders (Tables / Views / Materialized Views / Functions) start collapsed** but their children are already loaded — expand is purely visual.
- **Refresh** in the context menu fires `api.refreshSchemaCache(serverId, db)` and then collapses+clears every descendant of the DB so the next expand reads fresh data. Per `MILESTONES.md`: "Refresh on any node under a DB re-introspects that DB" — the refresh always operates at DB granularity.
- **Right-click context menu is a hand-rolled positioned `<ul>`**, not a library. Tauri 2 blocks the native context menu by default; intercepting `oncontextmenu={e => { e.preventDefault(); openMenu(...) }}` and rendering a popup is ~30 lines.
- **Password dialog** is a second `<dialog>` element (the first is the existing Add modal). Triggered from the server-node context menu's "Connect" item.
- **Selection** is by DB node: `selected = { serverId, database }`. Clicking a server selects `{ serverId, database: server.default_db }`. Clicking a deeper node walks up to find the enclosing DB. The right-pane DB input stays editable so users can override.
- **No drag-drop, no favorites, no virtualization.** A tree with tens of thousands of nodes (giant schemas) would benefit from windowing — explicitly v1.1.
- **No CSS framework or icon library.** Kind tags are plain text in brackets: `[T]` table, `[V]` view, `[M]` matview, `[P]` partitioned, `[F]` function, `[Proc]` procedure, `[Agg]` aggregate, `[Win]` window.
- **Expand/collapse triangles are CSS `::before` content** — `▸` collapsed, `▾` expanded. (Those are box-drawing arrows already in Unicode, not emoji — and they're the same characters that exist in the legacy M1.6 visual mock.)

## Deliverables

### 1. `src/lib/tauri.ts` — add types and five new API methods

Append the new types after the existing `ColumnMeta` block, and extend `CommandError.kind`, then add the new methods inside `api`. Full additions:

```ts
// ── Introspection types (mirrors introspect::*) ──

export type DatabaseInfo = { name: string };

export type RelationKind =
  | "table"
  | "view"
  | "matview"
  | "partitioned_table";

export type RelationInfo = { name: string; kind: RelationKind };

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

export type SchemaPayload = { v: number; schemas: SchemaInfoPayload[] };
```

Replace the `CommandError` type with the extended kind union:

```ts
export type CommandError = {
  kind:
    | "UnknownConnection"
    | "NotConnected"
    | "Slot"
    | "Pg"
    | "Store"
    | "Introspect"
    | "UnknownDatabase";
  message: string;
};
```

Extend `api` with five new methods (place below `getSlotState`):

```ts
  listDatabases: (serverId: number) =>
    invoke<DatabaseInfo[]>("list_databases", { serverId }),

  listSchemas: (serverId: number, database: string) =>
    invoke<string[]>("list_schemas", { serverId, database }),

  listRelations: (serverId: number, database: string, schema: string) =>
    invoke<RelationInfo[]>("list_relations", { serverId, database, schema }),

  listFunctions: (serverId: number, database: string, schema: string) =>
    invoke<FunctionInfo[]>("list_functions", { serverId, database, schema }),

  refreshSchemaCache: (serverId: number, database: string) =>
    invoke<SchemaPayload>("refresh_schema_cache", { serverId, database }),
```

### 2. `src/lib/tree.ts` — new file: node model + loaders

```ts
//! Tree node model + child loaders for the left-pane lazy tree.
//!
//! Nodes are plain TS objects; the parent `+page.svelte` keeps the root
//! list in a single `$state` so the deep reactivity proxies catch mutations
//! at every level.  We deliberately mutate fields directly instead of the
//! immutable-replacement pattern used in M1.6 — tree depth makes spreading
//! the whole subtree on every state change prohibitive.

import { api, type Connection, type RelationInfo, type FunctionInfo } from "./tauri";

// ── Node kinds ─────────────────────────────────────────────────────────

export type ServerNode = {
  kind: "server";
  conn: Connection;
  /** True while a request initiated by this node is in flight. */
  loading: boolean;
  /** Last error from a load triggered by this node; cleared on retry. */
  error: string | null;
  expanded: boolean;
  /** `null` until the user first expands. Empty array = loaded + no DBs. */
  children: DatabaseNode[] | null;
};

export type DatabaseNode = {
  kind: "database";
  serverId: number;
  name: string;
  loading: boolean;
  error: string | null;
  expanded: boolean;
  children: SchemaNode[] | null;
};

export type SchemaNode = {
  kind: "schema";
  serverId: number;
  database: string;
  name: string;
  loading: boolean;
  error: string | null;
  expanded: boolean;
  children: GroupNode[] | null;
};

export type GroupNode = {
  kind: "group";
  label: "Tables" | "Views" | "Materialized views" | "Partitioned tables" | "Functions";
  serverId: number;
  database: string;
  schema: string;
  expanded: boolean;
  children: LeafNode[];
};

export type LeafNode = {
  kind: "leaf";
  serverId: number;
  database: string;
  schema: string;
  name: string;
  /** RelationKind or FunctionKind (snake_case strings). */
  leafKind:
    | "table" | "view" | "matview" | "partitioned_table"
    | "function" | "procedure" | "aggregate" | "window";
};

export type TreeNode = ServerNode | DatabaseNode | SchemaNode | GroupNode | LeafNode;

// ── Loaders (called by Tree.svelte when expand fires) ──────────────────

/** Load the database list for a connected server. Mutates the node. */
export async function loadDatabases(node: ServerNode): Promise<void> {
  node.loading = true;
  node.error = null;
  try {
    const dbs = await api.listDatabases(node.conn.id);
    node.children = dbs.map((d) => ({
      kind: "database",
      serverId: node.conn.id,
      name: d.name,
      loading: false,
      error: null,
      expanded: false,
      children: null,
    }));
  } catch (e) {
    node.error = errorMessage(e);
    node.children = []; // mark as "attempted" so a retry is via Refresh
  } finally {
    node.loading = false;
  }
}

/** Load the schema list for a database. Mutates the node. */
export async function loadSchemas(node: DatabaseNode): Promise<void> {
  node.loading = true;
  node.error = null;
  try {
    const names = await api.listSchemas(node.serverId, node.name);
    node.children = names.map((name) => ({
      kind: "schema",
      serverId: node.serverId,
      database: node.name,
      name,
      loading: false,
      error: null,
      expanded: false,
      children: null,
    }));
  } catch (e) {
    node.error = errorMessage(e);
    node.children = [];
  } finally {
    node.loading = false;
  }
}

/** Load relations + functions for a schema in parallel, group them. */
export async function loadSchemaContents(node: SchemaNode): Promise<void> {
  node.loading = true;
  node.error = null;
  try {
    const [relations, functions] = await Promise.all([
      api.listRelations(node.serverId, node.database, node.name),
      api.listFunctions(node.serverId, node.database, node.name),
    ]);
    node.children = buildGroups(node, relations, functions);
  } catch (e) {
    node.error = errorMessage(e);
    node.children = [];
  } finally {
    node.loading = false;
  }
}

/** Group relations by kind and produce up to five group folders. Empty
 *  groups are omitted so a schema with only tables doesn't show three
 *  empty headers. */
function buildGroups(
  node: SchemaNode,
  relations: RelationInfo[],
  functions: FunctionInfo[],
): GroupNode[] {
  const groups: GroupNode[] = [];

  const leafFromRelation = (r: RelationInfo): LeafNode => ({
    kind: "leaf",
    serverId: node.serverId,
    database: node.database,
    schema: node.name,
    name: r.name,
    leafKind: r.kind,
  });
  const leafFromFunction = (f: FunctionInfo): LeafNode => ({
    kind: "leaf",
    serverId: node.serverId,
    database: node.database,
    schema: node.name,
    name: f.name,
    leafKind: f.kind,
  });

  const tables = relations.filter((r) => r.kind === "table").map(leafFromRelation);
  const views = relations.filter((r) => r.kind === "view").map(leafFromRelation);
  const matviews = relations.filter((r) => r.kind === "matview").map(leafFromRelation);
  const partitioned = relations.filter((r) => r.kind === "partitioned_table").map(leafFromRelation);
  const funcs = functions.map(leafFromFunction);

  const push = (label: GroupNode["label"], children: LeafNode[]) => {
    if (children.length === 0) return;
    groups.push({
      kind: "group",
      label,
      serverId: node.serverId,
      database: node.database,
      schema: node.name,
      expanded: true,
      children,
    });
  };

  push("Tables", tables);
  push("Views", views);
  push("Materialized views", matviews);
  push("Partitioned tables", partitioned);
  push("Functions", funcs);

  return groups;
}

/** Clear all loaded children of a database (and below). Used by Refresh
 *  so the next expand re-reads from the (just-refreshed) cache. */
export function clearDatabaseSubtree(node: DatabaseNode): void {
  node.children = null;
  node.expanded = false;
}

// ── Helpers ────────────────────────────────────────────────────────────

/** Pull a useful string out of a rejected `invoke` — handles the
 *  `CommandError` shape, fallback to `String(e)`. */
export function errorMessage(e: unknown): string {
  if (typeof e === "object" && e !== null && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  return String(e);
}

/** Short, single-character kind label rendered next to leaf names. */
export function kindTag(kind: LeafNode["leafKind"]): string {
  switch (kind) {
    case "table": return "[T]";
    case "view": return "[V]";
    case "matview": return "[M]";
    case "partitioned_table": return "[P]";
    case "function": return "[F]";
    case "procedure": return "[Proc]";
    case "aggregate": return "[Agg]";
    case "window": return "[Win]";
  }
}
```

### 3. `src/lib/Tree.svelte` — new file: recursive node renderer

```svelte
<script lang="ts">
  //! Recursive tree node component.
  //!
  //! One instance per node.  Self-recurses via <svelte:self> for children.
  //! Receives callbacks from the parent +page.svelte for:
  //!   - server selection (informs the right pane)
  //!   - context-menu open (parent positions the menu and dispatches actions)

  import type {
    TreeNode, ServerNode, DatabaseNode, SchemaNode, GroupNode, LeafNode,
  } from "./tree";
  import { loadDatabases, loadSchemas, loadSchemaContents, kindTag } from "./tree";

  type ContextMenuTarget = TreeNode;

  let {
    node,
    isConnected,
    selectedDb,
    onSelectDb,
    onContextMenu,
  }: {
    node: TreeNode;
    isConnected: (serverId: number) => boolean;
    selectedDb: { serverId: number; database: string } | null;
    onSelectDb: (serverId: number, database: string) => void;
    onContextMenu: (e: MouseEvent, target: ContextMenuTarget) => void;
  } = $props();

  async function toggleExpand() {
    if (node.kind === "leaf") return;

    // Server node: refuse to expand if disconnected.
    if (node.kind === "server" && !isConnected(node.conn.id)) {
      return;
    }

    node.expanded = !node.expanded;
    if (!node.expanded) return;

    // Lazy load if first expand.
    if (node.kind === "server" && node.children === null) {
      await loadDatabases(node);
    } else if (node.kind === "database" && node.children === null) {
      await loadSchemas(node);
    } else if (node.kind === "schema" && node.children === null) {
      await loadSchemaContents(node);
    }
    // Group nodes are pre-loaded; just toggle.
  }

  function onNodeClick() {
    if (node.kind === "database") {
      onSelectDb(node.serverId, node.name);
    } else if (node.kind === "schema" || node.kind === "leaf" || node.kind === "group") {
      // Inherit DB from the enclosing context.
      const sid = node.serverId;
      const db = "database" in node ? node.database : "";
      if (db) onSelectDb(sid, db);
    } else if (node.kind === "server") {
      // Selecting a server picks its default_db as the active DB.
      onSelectDb(node.conn.id, node.conn.default_db);
    }
  }

  function isSelected(): boolean {
    if (!selectedDb) return false;
    if (node.kind === "database") {
      return selectedDb.serverId === node.serverId && selectedDb.database === node.name;
    }
    return false;
  }

  function nodeLabel(): string {
    switch (node.kind) {
      case "server": return node.conn.name;
      case "database": return node.name;
      case "schema": return node.name;
      case "group": return `${node.label} (${node.children.length})`;
      case "leaf": return `${kindTag(node.leafKind)} ${node.name}`;
    }
  }

  function arrow(): string {
    if (node.kind === "leaf") return "  ";
    return node.expanded ? "▾" : "▸";
  }
</script>

<div class="tree-row" oncontextmenu={(e) => onContextMenu(e, node)}>
  <button
    type="button"
    class="row-button"
    class:selected={isSelected()}
    onclick={() => { onNodeClick(); toggleExpand(); }}
  >
    <span class="arrow">{arrow()}</span>
    <span class="label">{nodeLabel()}</span>
    {#if "loading" in node && node.loading}
      <span class="loading">…</span>
    {/if}
    {#if "error" in node && node.error}
      <span class="error" title={node.error}>!</span>
    {/if}
  </button>
</div>

{#if "expanded" in node && node.expanded && "children" in node && node.children}
  <div class="children">
    {#each node.children as child (childKey(child))}
      <svelte:self
        node={child}
        {isConnected}
        {selectedDb}
        {onSelectDb}
        {onContextMenu}
      />
    {/each}
  </div>
{/if}

<script lang="ts" context="module">
  // Stable key per node — used by the {#each (key)} expression for proper
  // diffing when children are mutated.  Plain index would lose component
  // state on insertion/removal.
  export function childKey(n: TreeNode): string {
    switch (n.kind) {
      case "server": return `server:${n.conn.id}`;
      case "database": return `db:${n.serverId}:${n.name}`;
      case "schema": return `schema:${n.serverId}:${n.database}:${n.name}`;
      case "group": return `group:${n.serverId}:${n.database}:${n.schema}:${n.label}`;
      case "leaf": return `leaf:${n.serverId}:${n.database}:${n.schema}:${n.leafKind}:${n.name}`;
    }
  }
</script>

<style>
  .tree-row {
    display: block;
  }
  .row-button {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    width: 100%;
    padding: 0.15rem 0.3rem;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 3px;
    font: inherit;
    font-size: 0.9rem;
    color: inherit;
    text-align: left;
    cursor: pointer;
  }
  .row-button:hover {
    background: #e8e8e8;
  }
  .row-button.selected {
    background: #d0d0ff;
    border-color: #8888cc;
  }
  .arrow {
    font-family: monospace;
    width: 0.9rem;
    text-align: center;
    color: #666;
    font-size: 0.85rem;
  }
  .label { flex: 1; }
  .loading { color: #888; font-style: italic; }
  .error { color: #cc0000; font-weight: bold; cursor: help; }
  .children {
    padding-left: 1.2rem;
    border-left: 1px solid #e0e0e0;
    margin-left: 0.4rem;
  }
</style>
```

**Note on `<svelte:self>`**: Svelte 5 still supports recursive component self-references via `<svelte:self>`. It is the idiomatic way to render a tree without splitting into a separate "TreeNode" file. If a future Svelte release deprecates it, the fix is mechanical — extract the body into `TreeNode.svelte` and import it here.

### 4. `src/routes/+page.svelte` — rewrite

Replace the entire file. The right pane (SQL form + result) is preserved structurally; only the left pane and supporting state change.

```svelte
<script lang="ts">
  import {
    api,
    type Connection,
    type NewConnection,
    type SlotState,
    type QueryResult,
    type CommandError,
  } from "$lib/tauri";
  import Tree from "$lib/Tree.svelte";
  import type { ServerNode, TreeNode, DatabaseNode } from "$lib/tree";
  import { clearDatabaseSubtree, errorMessage } from "$lib/tree";

  // ═════════════════ State ═════════════════

  let connections = $state<Connection[]>([]);
  let connectedState = $state<Record<number, SlotState>>({});

  // Tree root: one ServerNode per saved connection.
  let tree = $state<ServerNode[]>([]);

  // Right-pane target.  Set by clicking a tree node.
  let selectedDb = $state<{ serverId: number; database: string } | null>(null);

  // Add-connection modal (unchanged from M1.6).
  let addDialog = $state<HTMLDialogElement | null>(null);
  let addForm: NewConnection = $state(defaultAddForm());
  let addError = $state("");

  // Password dialog (new).
  let pwDialog = $state<HTMLDialogElement | null>(null);
  let pwTargetId = $state<number | null>(null);
  let pwPassword = $state("");
  let pwError = $state("");

  // Context menu (new).
  let menu = $state<{
    x: number;
    y: number;
    target: TreeNode;
  } | null>(null);

  // SQL form (kept from M1.6).
  let sql = $state("SELECT 1");
  let runningQuery = $state(false);
  let result = $state<QueryResult | { error: CommandError } | null>(null);

  // ═════════════════ Initial load ═════════════════

  $effect(() => {
    api.listConnections().then((rows) => {
      connections = rows;
      tree = rows.map(makeServerNode);
    });
  });

  function makeServerNode(c: Connection): ServerNode {
    return {
      kind: "server",
      conn: c,
      loading: false,
      error: null,
      expanded: false,
      children: null,
    };
  }

  function defaultAddForm(): NewConnection {
    return {
      name: "",
      host: "localhost",
      port: 5432,
      default_db: "postgres",
      username: "postgres",
      ssl_mode: "disable",
      slot_budget: 2,
      password_ref: null,
    };
  }

  // ═════════════════ Helpers ═════════════════

  function isConnected(id: number): boolean {
    return id in connectedState;
  }

  function slotLabel(s: SlotState | undefined): string {
    if (!s) return "";
    const busy = s.slots.filter((x) => x.busy).length;
    return `[${busy}/${s.budget}]`;
  }

  // ═════════════════ Add connection (unchanged shape) ═════════════════

  function openAddModal() {
    addForm = defaultAddForm();
    addError = "";
    addDialog?.showModal();
  }

  async function saveConnection(e: Event) {
    e.preventDefault();
    addError = "";
    try {
      await api.saveConnection({ ...addForm });
      const rows = await api.listConnections();
      connections = rows;
      tree = rows.map(makeServerNode);
      addDialog?.close();
    } catch (err) {
      addError = errorMessage(err);
    }
  }

  // ═════════════════ Connect / disconnect ═════════════════

  function promptPassword(id: number) {
    pwTargetId = id;
    pwPassword = "";
    pwError = "";
    pwDialog?.showModal();
  }

  async function submitPassword(e: Event) {
    e.preventDefault();
    if (pwTargetId === null) return;
    pwError = "";
    try {
      const state = await api.connectServer(pwTargetId, pwPassword);
      connectedState[pwTargetId] = state;
      pwDialog?.close();
    } catch (err) {
      pwError = errorMessage(err);
    }
  }

  async function disconnect(id: number) {
    await api.disconnectServer(id);
    delete connectedState[id];
    // Collapse the tree subtree for this server so a future Connect starts fresh.
    const node = tree.find((n) => n.conn.id === id);
    if (node) {
      node.children = null;
      node.expanded = false;
    }
    if (selectedDb?.serverId === id) selectedDb = null;
  }

  async function deleteConn(id: number) {
    await api.deleteConnection(id);
    delete connectedState[id];
    const rows = await api.listConnections();
    connections = rows;
    tree = rows.map(makeServerNode);
    if (selectedDb?.serverId === id) selectedDb = null;
  }

  // ═════════════════ Refresh ═════════════════

  /** Walk up to the enclosing DatabaseNode for the menu target, then
   *  re-introspect that DB and clear the subtree. */
  async function refreshFromTarget(target: TreeNode) {
    const dbNode = findEnclosingDatabaseNode(target);
    if (!dbNode) return;
    dbNode.loading = true;
    dbNode.error = null;
    try {
      await api.refreshSchemaCache(dbNode.serverId, dbNode.name);
      clearDatabaseSubtree(dbNode);
    } catch (err) {
      dbNode.error = errorMessage(err);
    } finally {
      dbNode.loading = false;
    }
  }

  function findEnclosingDatabaseNode(target: TreeNode): DatabaseNode | null {
    if (target.kind === "database") return target;
    if (target.kind === "server") return null; // server context: nothing to refresh
    // Find via serverId + database string.
    const sid = "serverId" in target ? target.serverId : -1;
    const db = "database" in target ? target.database : "";
    const server = tree.find((n) => n.conn.id === sid);
    return server?.children?.find((d) => d.name === db) ?? null;
  }

  // ═════════════════ Selection ═════════════════

  function selectDb(serverId: number, database: string) {
    selectedDb = { serverId, database };
  }

  let selectedConn = $derived(
    selectedDb ? connections.find((c) => c.id === selectedDb!.serverId) ?? null : null,
  );

  // ═════════════════ Context menu ═════════════════

  function openMenu(e: MouseEvent, target: TreeNode) {
    e.preventDefault();
    menu = { x: e.clientX, y: e.clientY, target };
  }

  function closeMenu() {
    menu = null;
  }

  async function menuAction(action: string) {
    if (!menu) return;
    const target = menu.target;
    closeMenu();

    switch (action) {
      case "connect":
        if (target.kind === "server") promptPassword(target.conn.id);
        break;
      case "disconnect":
        if (target.kind === "server") await disconnect(target.conn.id);
        break;
      case "refresh":
        await refreshFromTarget(target);
        break;
      case "copy-name": {
        const text = qualifiedName(target);
        if (text) navigator.clipboard.writeText(text);
        break;
      }
      case "delete":
        if (target.kind === "server") await deleteConn(target.conn.id);
        break;
    }
  }

  function qualifiedName(t: TreeNode): string {
    switch (t.kind) {
      case "server": return t.conn.name;
      case "database": return t.name;
      case "schema": return `${t.database}.${t.name}`;
      case "group": return ""; // not meaningful
      case "leaf": return `${t.schema}.${t.name}`;
    }
  }

  // What menu items apply to this target?
  function menuItemsFor(t: TreeNode): { action: string; label: string }[] {
    const items: { action: string; label: string }[] = [];
    if (t.kind === "server") {
      if (isConnected(t.conn.id)) {
        items.push({ action: "disconnect", label: "Disconnect" });
      } else {
        items.push({ action: "connect", label: "Connect…" });
      }
      items.push({ action: "copy-name", label: "Copy name" });
      items.push({ action: "delete", label: "Delete connection" });
    } else if (t.kind === "database") {
      items.push({ action: "refresh", label: "Refresh schema" });
      items.push({ action: "copy-name", label: "Copy name" });
    } else if (t.kind === "schema" || t.kind === "leaf") {
      items.push({ action: "refresh", label: "Refresh schema" });
      items.push({ action: "copy-name", label: "Copy qualified name" });
    } else if (t.kind === "group") {
      items.push({ action: "refresh", label: "Refresh schema" });
    }
    return items;
  }

  // ═════════════════ Query ═════════════════

  async function run() {
    if (!selectedDb || !isConnected(selectedDb.serverId) || !sql.trim() || runningQuery) return;
    runningQuery = true;
    result = null;
    try {
      result = await api.runQuery(selectedDb.serverId, selectedDb.database, sql);
    } catch (err) {
      result = { error: err as CommandError };
    } finally {
      runningQuery = false;
    }
  }

  let canRun = $derived(
    selectedDb !== null &&
      isConnected(selectedDb.serverId) &&
      sql.trim().length > 0 &&
      !runningQuery,
  );

  function renderResult(r: QueryResult): string {
    if (r.columns.length === 0)
      return `(no columns)\n${r.row_count} rows, ${r.duration_ms}ms`;
    const header = r.columns.map((c) => c.name).join("\t");
    const lines = r.rows.map((row) =>
      row
        .map((cell) => {
          if (cell === null) return "NULL";
          if (typeof cell === "object") return JSON.stringify(cell);
          return String(cell);
        })
        .join("\t"),
    );
    return [header, ...lines, "", `${r.row_count} rows in ${r.duration_ms}ms`].join("\n");
  }
</script>

<svelte:window onclick={closeMenu} oncontextmenu={(e) => { if (!menu) return; e.preventDefault(); closeMenu(); }} />

<div class="shell">
  <!-- ═══════ LEFT PANE ═══════ -->
  <aside class="left-pane">
    <div class="header-row">
      <h2>Connections</h2>
      <button class="btn" onclick={openAddModal}>+ Add</button>
    </div>

    {#if tree.length === 0}
      <p class="muted">No saved connections.</p>
    {:else}
      <div class="tree">
        {#each tree as serverNode (serverNode.conn.id)}
          <div class="server-block">
            <div class="server-row">
              <Tree
                node={serverNode}
                {isConnected}
                {selectedDb}
                onSelectDb={selectDb}
                onContextMenu={openMenu}
              />
              <span class="slot-badge">{slotLabel(connectedState[serverNode.conn.id])}</span>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </aside>

  <!-- ═══════ RIGHT PANE ═══════ -->
  <main class="right-pane">
    {#if selectedConn && selectedDb}
      {@const sd = selectedDb}
      <h3>{selectedConn.name} / {sd.database}</h3>

      <textarea
        bind:value={sql}
        class="sql-input"
        rows={8}
        placeholder="SELECT 1"
      ></textarea>

      <button class="btn" onclick={run} disabled={!canRun}>
        {runningQuery ? "Running…" : "Run"}
      </button>

      {#if !isConnected(sd.serverId)}
        <p class="muted">Not connected. Right-click the server in the tree → Connect.</p>
      {/if}

      {#if result}
        <div class="result-area">
          {#if "error" in result}
            <pre class="error">[{result.error.kind}] {result.error.message}</pre>
          {:else}
            <pre>{renderResult(result)}</pre>
          {/if}
        </div>
      {/if}
    {:else}
      <p class="muted">Select a database in the tree (left) to start querying.</p>
    {/if}
  </main>
</div>

<!-- ═══════ CONTEXT MENU ═══════ -->
{#if menu}
  {@const items = menuItemsFor(menu.target)}
  <ul
    class="context-menu"
    style="left: {menu.x}px; top: {menu.y}px;"
    onclick={(e) => e.stopPropagation()}
    oncontextmenu={(e) => e.preventDefault()}
  >
    {#each items as item}
      <li>
        <button class="menu-item" onclick={() => menuAction(item.action)}>{item.label}</button>
      </li>
    {/each}
  </ul>
{/if}

<!-- ═══════ ADD-CONNECTION DIALOG (unchanged from M1.6 shape) ═══════ -->
<dialog bind:this={addDialog} class="modal">
  <h2>Add Connection</h2>
  <form onsubmit={saveConnection} class="add-form">
    <label class="field">Name<input class="input" bind:value={addForm.name} required /></label>
    <label class="field">Host<input class="input" bind:value={addForm.host} required /></label>
    <label class="field">Port<input class="input" type="number" min={1} max={65535} bind:value={addForm.port} /></label>
    <label class="field">Default database<input class="input" bind:value={addForm.default_db} required /></label>
    <label class="field">Username<input class="input" bind:value={addForm.username} required /></label>
    <label class="field">
      SSL mode
      <select class="input" bind:value={addForm.ssl_mode}>
        <option value="disable">disable</option>
        <option value="prefer">prefer</option>
        <option value="require">require</option>
        <option value="verify-ca">verify-ca</option>
        <option value="verify-full">verify-full</option>
      </select>
    </label>
    <label class="field">Slot budget<input class="input" type="number" min={1} max={16} bind:value={addForm.slot_budget} /></label>

    {#if addError}<p class="error">{addError}</p>{/if}

    <div class="modal-actions">
      <button type="button" class="btn" onclick={() => addDialog?.close()}>Cancel</button>
      <button type="submit" class="btn btn-primary">Save</button>
    </div>
  </form>
</dialog>

<!-- ═══════ PASSWORD DIALOG ═══════ -->
<dialog bind:this={pwDialog} class="modal">
  <h2>Password</h2>
  <form onsubmit={submitPassword} class="add-form">
    <label class="field">
      Password
      <input type="password" class="input" bind:value={pwPassword} autofocus />
    </label>
    {#if pwError}<p class="error">{pwError}</p>{/if}
    <div class="modal-actions">
      <button type="button" class="btn" onclick={() => pwDialog?.close()}>Cancel</button>
      <button type="submit" class="btn btn-primary">Connect</button>
    </div>
  </form>
</dialog>

<style>
  .shell { display: flex; height: 100vh; }
  .left-pane { width: 320px; min-width: 320px; border-right: 1px solid #ccc; padding: 0.75rem; overflow-y: auto; display: flex; flex-direction: column; gap: 0.5rem; }
  .right-pane { flex: 1; padding: 1rem; overflow-y: auto; display: flex; flex-direction: column; gap: 0.5rem; }

  .header-row { display: flex; align-items: center; justify-content: space-between; }
  h2, h3 { margin: 0; font-size: 1.05rem; }

  .tree { display: flex; flex-direction: column; }
  .server-block { display: flex; flex-direction: column; }
  .server-row { display: flex; align-items: center; justify-content: space-between; gap: 0.5rem; }
  .slot-badge { font-size: 0.8rem; color: #666; font-variant-numeric: tabular-nums; padding-right: 0.3rem; }

  .btn { padding: 0.3rem 0.6rem; border: 1px solid #888; border-radius: 4px; background: #f0f0f0; cursor: pointer; font: inherit; font-size: 0.9rem; }
  .btn:hover { background: #e0e0e0; }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-primary { background: #3366cc; color: white; border-color: #2255aa; }
  .btn-primary:hover { background: #2255aa; }

  .input { padding: 0.35rem; border: 1px solid #aaa; border-radius: 4px; font: inherit; box-sizing: border-box; }

  .sql-input { width: 100%; padding: 0.5rem; font: 14px monospace; border: 1px solid #aaa; border-radius: 4px; box-sizing: border-box; resize: vertical; }

  .result-area { border-top: 1px solid #ccc; padding-top: 0.5rem; }
  .result-area pre { margin: 0; font: 13px monospace; white-space: pre-wrap; }
  .error { color: #cc0000; }

  .modal { border: 1px solid #888; border-radius: 8px; padding: 1.25rem; max-width: 400px; width: 90%; }
  .modal::backdrop { background: rgba(0,0,0,0.3); }
  .add-form { display: flex; flex-direction: column; gap: 0.5rem; }
  .field { display: flex; flex-direction: column; gap: 0.15rem; font-size: 0.9rem; }
  .modal-actions { display: flex; justify-content: flex-end; gap: 0.5rem; margin-top: 0.5rem; }

  .muted { color: #888; font-style: italic; }

  .context-menu {
    position: fixed;
    list-style: none;
    margin: 0;
    padding: 0.25rem 0;
    background: white;
    border: 1px solid #888;
    border-radius: 4px;
    box-shadow: 0 2px 8px rgba(0,0,0,0.2);
    min-width: 180px;
    z-index: 100;
  }
  .menu-item {
    display: block;
    width: 100%;
    padding: 0.35rem 0.75rem;
    background: transparent;
    border: none;
    text-align: left;
    cursor: pointer;
    font: inherit;
    font-size: 0.9rem;
  }
  .menu-item:hover { background: #e0e0ff; }
</style>
```

## Implementation order

1. **`src/lib/tauri.ts`** — add the new types, extend `CommandError.kind`, append the five new `api` methods. Verify with `pnpm check` (no UI changes yet, but type-checking the bridge in isolation catches the simplest mistakes).
2. **`src/lib/tree.ts`** — create the new file. No imports from any Svelte component yet, so it builds independently. Run `pnpm check`.
3. **`src/lib/Tree.svelte`** — create the new file. Imports from `tree.ts` and uses `<svelte:self>`. Run `pnpm check`.
4. **`src/routes/+page.svelte`** — rewrite. Imports `Tree` and the helpers from `tree.ts`. Run `pnpm check`; then `./run.sh` against a real Postgres to walk the manual smoke test.

## Known gotchas

- **`<svelte:self>` in a Svelte 5 component.** Still supported as of Svelte 5; renders a fresh instance of the component bound to a different `node`. The `{#each ... (key)}` block must use the stable per-node key (`childKey`) so component identity tracks the node identity across mutations. If `<svelte:self>` is removed in a future Svelte version, extract `Tree.svelte`'s template into `TreeNode.svelte` and import it from `Tree.svelte`.
- **Svelte 5 `$state` is deeply reactive via Proxy.** Mutating `node.expanded = true` on a node *inside* the `$state<ServerNode[]>` tree triggers re-render. This is opposite to what M1.6's `Record<number, SlotState>` example suggested; M1.6's pattern was a stylistic choice, not a framework requirement. For a tree this deep, mutation is the only sane option. If reactivity ever fails to fire after a mutation, the field is probably typed as `readonly` somewhere — drop the qualifier.
- **`navigator.clipboard.writeText` requires a secure context.** In Tauri's webview this is always available (Tauri serves over a custom scheme treated as secure). No special permission needed.
- **Tauri 2 disables the native context menu in the webview by default.** Intercepting `oncontextmenu` on the row and rendering a custom `<ul.context-menu>` works because no native menu fights us. The `<svelte:window oncontextmenu>` handler dismisses the menu on a second right-click anywhere else; the regular `onclick` closes it on left-click.
- **`<svelte:window onclick={closeMenu}>` fires on every click — including the one that opens the menu.** Because the menu is opened on `oncontextmenu` (right click) and `onclick` is left click, they don't overlap. The first left-click after the menu opens dismisses it; if that click was on a menu item, the `e.stopPropagation()` on the `<ul>` prevents the window handler from firing before the item's `onclick` runs.
- **Reactive `connectedState` mutation.** This stays as the M1.6 pattern: `connectedState[id] = state` for sets, `delete connectedState[id]` for removes. Svelte 5's proxy catches both — but if you see stale UI, fall back to the spread/replace pattern.
- **`SchemaPayload` doesn't appear in the tree directly.** The bridge exposes the type because `refreshSchemaCache` returns it, but the tree drops the result and just clears the DB subtree so the next expand re-fetches via `list_schemas` (which hits the now-fresh cache).
- **Loading state on a Refresh.** The Refresh action sets `dbNode.loading = true` until `refreshSchemaCache` resolves *and* clears the subtree. The `…` spinner appears next to the DB node row during this time. If `refreshSchemaCache` fails, the error sits on the DB node and the subtree is preserved (the user sees stale data but knows the refresh didn't take).
- **Expand on a disconnected server is silently refused.** `toggleExpand` returns early. The user-friendly path is: right-click → Connect, then expand. A future polish (M6) could show a tooltip; v1 just no-ops.
- **A server with `loading = true` still shows the slot badge.** The badge is fed by `connectedState[id]`, not by `loading`. Connect populates `connectedState[id]` synchronously inside the password dialog's submit handler, so the badge appears immediately after a successful connect.
- **Selecting a deep node sets `selectedDb` to the enclosing DB.** Clicking a leaf in `public.users` targets the right pane at `(server, postgres)` if the schema is in `postgres`. Clicking a *schema* node does the same — only the DB matters for query execution.
- **`tree` and `connections` can drift.** Saving a new connection re-fetches `connections` and re-builds `tree` from scratch — this is intentional and discards any expanded state. The cost is acceptable in v1; persisting expand-state across re-saves is a v1.1 polish. Deleting a connection also re-builds the tree.
- **The right pane no longer has its own DB input.** It's driven by `selectedDb.database`. If the user wants a different DB on the same server, they click that DB node in the tree. (Reintroducing a free-form DB input is a known M6 polish.)
- **`@const` inside `{#if selectedConn && selectedDb}`** captures the narrowed `selectedDb` so TypeScript inside the right-pane block treats it as non-null. Without `{@const sd = selectedDb}`, `svelte-check` flags `selectedDb!.serverId` references inside event handlers as possibly null.
- **Tauri 2 invoke args are camelCase at the boundary but snake_case inside structs.** The five new methods follow this exactly: `serverId` (rename), `database`/`schema` (single-word, no rename), and the returned object fields are snake_case. Mismatches surface as Tauri returning `null` for the field.
- **A11y warnings.** `svelte-check` will flag the `<button>` rows as ok and the `<div class="server-row">` as fine, but the inline `<ul class="context-menu">` may trip `a11y_click_events_have_key_events` if the menu-item buttons don't have keyboard handlers. The buttons inside are real `<button>` elements so Enter/Space already work; add `<!-- svelte-ignore -->` only if a specific warning shows up. **Do not** disable warnings preemptively.
- **`autofocus` on the password input** is a deliberate UX choice — when the dialog opens, focus jumps to the password field. Svelte 5 forwards the HTML attribute correctly. If `svelte-check` warns, suppress with `<!-- svelte-ignore a11y_autofocus -->`.
- **`tree.find((n) => n.conn.id === sid)`** is O(n) per call. Acceptable for tens of connections; switch to a `Map` if a future user has hundreds.
- **No new `pnpm` deps.** Don't add `tippy.js`, `floating-ui`, `framer-motion`, or any tree library.

## Tests

There are no automated frontend tests in M2.4. `./test.sh` runs `pnpm check` (TypeScript + Svelte), which must pass; the manual smoke test below covers behaviour.

### Manual smoke test

Against a local Docker Postgres:

```bash
docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17
docker exec -it quill-pg psql -U postgres -c "CREATE DATABASE analytics; \
                                              CREATE TABLE public.users (id int); \
                                              CREATE VIEW public.user_count AS SELECT count(*) FROM public.users; \
                                              CREATE FUNCTION public.now_utc() RETURNS timestamptz LANGUAGE sql AS 'SELECT now()';"
./run.sh
```

1. Window opens with the two-pane layout. Left pane shows "No saved connections" until you add one.
2. Click **+ Add**. Fill name=`local`, host=`localhost`, port=`5432`, default_db=`postgres`, username=`postgres`, ssl_mode=`disable`, slot_budget=`2`. Save.
3. The connection `local` appears in the left pane with a `▸` triangle and no slot indicator (not connected).
4. Click `local` once. Right pane updates to `local / postgres` (default_db). The pane says "Not connected. Right-click → Connect."
5. **Right-click** `local` → menu shows Connect / Copy name / Delete. Click **Connect…**
6. Password dialog opens. Type `dev`, click Connect. Dialog closes. Slot indicator appears next to `local`: `[0/2]`.
7. Click the `▸` next to `local` (or click `local` again). A `…` flashes; then the databases load: `analytics`, `postgres` (no `template0`, no `template1`).
8. Click `▸` next to `postgres`. A `…` flashes (slot bumps to `[1/2]` during introspection); then schemas appear: `public` (no `pg_*`, no `information_schema`).
9. Click `▸` next to `public`. Tables/Views/Functions group folders appear (a second `…` may flash, but the cache is warm so this should be near-instant).
10. Verify: Tables → `users`; Views → `user_count`; Functions → `now_utc`. Kind tags `[T]`, `[V]`, `[F]` show next to names.
11. Right-click `public` → **Refresh schema**. A `…` flashes next to `postgres` (the enclosing DB). After it clears, the `postgres` subtree is collapsed and shows `▸`.
12. Re-expand the path. Children load from cache (no slot bump).
13. Right-click `users` → **Copy qualified name** → paste somewhere shows `public.users`.
14. Click the `postgres` database node. Right pane changes to `local / postgres`. Edit the textarea to `SELECT * FROM public.users LIMIT 5;` and click Run. Result appears in `<pre>`.
15. Click `analytics` in the tree (its `▸` if collapsed). Right pane switches to `local / analytics`. Run a query against analytics — should hit a different slot or evict per the LRU rules from M1.
16. Right-click `local` → **Disconnect**. Slot indicator disappears; the tree subtree collapses; the right pane shows the disconnected message.
17. Close the window. No crashes.

## Acceptance criteria

- [ ] `./test.sh` succeeds — `cargo test` (no Rust changes in this task) plus `pnpm check` (TypeScript + Svelte type-check) both pass.
- [ ] `grep -E "on:click|on:submit|on:keydown|on:change|on:input" src/routes/+page.svelte src/lib/Tree.svelte` returns zero matches (Svelte 5 syntax only).
- [ ] `grep -F '$:' src/routes/+page.svelte src/lib/Tree.svelte` returns zero matches.
- [ ] `ls src/lib/` shows `tauri.ts`, `tree.ts`, `Tree.svelte` (no other new files).
- [ ] `git diff src-tauri/` is empty (M2.4 is frontend-only).
- [ ] `grep -c "invoke<" src/lib/tauri.ts` returns `12` (7 from M1.5 + 5 new).
- [ ] `grep -F "<svelte:self>" src/lib/Tree.svelte` returns at least one match.
- [ ] `grep -F "Promise.all" src/lib/tree.ts` returns at least one match (the schema-contents loader runs relations + functions in parallel).
- [ ] Manual smoke test (above) passes against a fresh Docker Postgres with the provided fixture.
- [ ] On step 7 of the smoke test, the slot badge visibly bumps to `[1/2]` while the introspection query runs, and returns to `[0/2]` once the children appear.
- [ ] On step 11 (Refresh), the slot bumps again — `refresh_schema_cache` always re-introspects regardless of cache freshness.
- [ ] On step 9 (re-expand after warm cache), the slot does **not** bump — the second `list_schemas` is a pure cache read.

## Out of scope

- CodeMirror editor — **M3**.
- Result grid (sortable, resizable, cell preview) — **M3**.
- Query cancellation, Cancel button — **M3**.
- Autocomplete — **M4**.
- Query tabs, history panel, saved queries, CSV export — **M5**.
- OS keychain integration, dark mode, settings panel, visual polish — **M6**.
- Persisting tree expand state across app restarts — **M6** (settings/state persistence).
- Persisting tree expand state across "Add connection" / "Delete connection" — explicitly accepted as discarded.
- Drag-drop, favorites, search-in-tree, virtualization — **v1.1** at the earliest.
- A11y deep-dive (full keyboard nav of the tree, ARIA tree role) — **M6** polish.
- Tooltips on slot indicator (per-slot DB/busy detail) — **M6**.
- Editing a saved connection — context menu has Delete only in v1; Edit is **M6**.
- Re-introducing a free-form DB input in the right pane — **M6** if a user asks; v1 is tree-driven.
- Function arg signatures, source code, return type tooltips — **M4**+ as part of autocomplete metadata.
