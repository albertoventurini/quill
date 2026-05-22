//! Tree node model + child loaders for the left-pane lazy tree.
//!
//! Nodes are plain TS objects; the parent `+page.svelte` keeps the root
//! list in a single `$state` so the deep reactivity proxies catch mutations
//! at every level.  We deliberately mutate fields directly instead of the
//! immutable-replacement pattern used in M1.6 — tree depth makes spreading
//! the whole subtree on every state change prohibitive.

import { api, type Connection, type RelationInfo, type FunctionInfo } from "./tauri";
import { clearSchemaPayload } from "./schemaStore";

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
 *  so the next expand re-reads from the (just-refreshed) cache.  Also
 *  drops the M4.4 schema-store entry for the same `(serverId, database)`
 *  so the autocomplete source picks up the refreshed payload on its next
 *  trigger.  */
export function clearDatabaseSubtree(node: DatabaseNode): void {
  node.children = null;
  node.expanded = false;
  clearSchemaPayload(node.serverId, node.name);
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
