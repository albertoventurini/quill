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
    onConnectServer,
    slotLabel,
    expiryRemainingMs,
  }: {
    node: TreeNode;
    isConnected: (serverId: number) => boolean;
    selectedDb: { serverId: number; database: string } | null;
    onSelectDb: (serverId: number, database: string) => void;
    onContextMenu: (e: MouseEvent, target: ContextMenuTarget) => void;
    onConnectServer?: (id: number) => void;
    slotLabel?: string;
    expiryRemainingMs?: number;
  } = $props();

  async function toggleExpand() {
    if (node.kind === "column") return;

    // Relation leaves expand to show their columns (already loaded with the
    // schema payload — no fetch). Function leaves have no columns.
    if (node.kind === "leaf") {
      if (node.children && node.children.length > 0) {
        node.expanded = !node.expanded;
      }
      return;
    }

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
    } else if (
      node.kind === "schema" || node.kind === "leaf" ||
      node.kind === "group" || node.kind === "column"
    ) {
      // Inherit DB from the enclosing context.
      const sid = node.serverId;
      const db = "database" in node ? node.database : "";
      if (db) onSelectDb(sid, db);
    } else if (node.kind === "server") {
      if (!isConnected(node.conn.id)) {
        onConnectServer?.(node.conn.id);
        return;
      }
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
      case "leaf": {
        const tag = kindTag(node.leafKind);
        return tag ? `${tag} ${node.name}` : node.name;
      }
      case "column": return node.name;
    }
  }

  // Label shown in the children area once a load resolves with no rows.
  // Distinct per level so "empty" never reads as "still loading".
  function emptyLabel(): string {
    switch (node.kind) {
      case "server": return "No databases";
      case "database": return "No schemas";
      case "schema": return "No tables or functions — right-click to Refresh schema";
      default: return "Empty";
    }
  }

  function arrow(): string {
    if (node.kind === "column") return "  ";
    if (node.kind === "leaf") {
      return node.children && node.children.length > 0
        ? (node.expanded ? "▾" : "▸")
        : "  ";
    }
    return node.expanded ? "▾" : "▸";
  }
</script>

<div class="tree-node">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="tree-row" oncontextmenu={(e) => onContextMenu(e, node)}>
    <button
      type="button"
      class="row-button"
      class:selected={isSelected()}
      onclick={() => { onNodeClick(); toggleExpand(); }}
    >
      <span class="arrow">{arrow()}</span>
      <span
        class="label label-{node.kind}"
        class:col-name={node.kind === "column"}
        class:server-connected={node.kind === "server" && isConnected(node.conn.id)}
      >{nodeLabel()}</span>
      {#if node.kind === "column"}
        <span class="coltype">{node.typeName}</span>
        {#if node.notNull}<span class="notnull">not null</span>{/if}
      {/if}
      {#if node.kind === "server" && slotLabel}
        <span class="slot-badge">{slotLabel}</span>
      {/if}
      {#if node.kind === "server" && expiryRemainingMs !== undefined && expiryRemainingMs < 300_000}
        {#if expiryRemainingMs < 60_000}
          <span class="expiry expiry-critical">expires in {Math.ceil(expiryRemainingMs / 1000)}s</span>
        {:else}
          <span class="expiry expiry-warn">expires in {Math.ceil(expiryRemainingMs / 60_000)}m</span>
        {/if}
      {/if}
      {#if "loading" in node && node.loading}
        <span class="loading">…</span>
      {/if}
      {#if "error" in node && node.error}
        <span class="error" title={node.error}>!</span>
      {/if}
    </button>
  </div>

  {#if "expanded" in node && node.expanded && "children" in node}
    <div class="children">
      {#if ("loading" in node && node.loading) || node.children === null}
        <span class="status-hint loading-hint">Loading…</span>
      {:else if "error" in node && node.error}
        <span class="status-hint error-hint" title={node.error}>Couldn’t load — {node.error}</span>
      {:else if node.children.length === 0}
        <span class="status-hint empty-hint">{emptyLabel()}</span>
      {:else}
        {#each node.children as child (childKey(child))}
          <!-- svelte-ignore svelte_self_deprecated -->
          <svelte:self
            node={child}
            {isConnected}
            {selectedDb}
            {onSelectDb}
            {onContextMenu}
            {onConnectServer}
          />
        {/each}
      {/if}
    </div>
  {/if}
</div>

<script lang="ts" module>
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
      case "column": return `col:${n.serverId}:${n.database}:${n.schema}:${n.relation}:${n.name}`;
    }
  }
</script>

<style>
  .tree-node {
    display: flex;
    flex-direction: column;
    color: var(--text-primary);
  }
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
    background: var(--bg-hover);
  }
  .row-button.selected {
    background: var(--bg-selected);
    border-color: var(--border-accent);
  }
  .arrow {
    font-family: monospace;
    width: 0.9rem;
    flex: 0 0 auto;
    text-align: center;
    color: var(--text-mid);
    font-size: 0.85rem;
  }
  .label { flex: 1; min-width: 0; }
  .label.col-name { flex: 0 1 auto; }

  /* Per-kind label styling builds a visual hierarchy so structural
     meta-labels never read like selectable DB objects. */
  /* Bold only once connected, so connection state is readable at a glance. */
  .label-server { font-weight: 400; color: var(--text-heading); }
  .label-server.server-connected { font-weight: 600; }
  .label-database { color: var(--text-db); }
  /* Schema, table and column all use the primary text colour; the hierarchy
     reads from indentation, the dimmed group headers, and the coloured
     database/server rows above — not from per-level shading. Weight stays the
     server-connected cue; the font is sans-serif throughout. */
  .label-schema,
  .label-leaf,
  .label-column { color: var(--text-primary); }
  /* Group headers ("Tables", "Views", "Functions", …): uppercase, dimmed
     and tracked out so they read as section labels, not data. */
  .label-group {
    font-size: 0.72rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-faint);
  }
  .coltype {
    font-size: 0.8rem;
    color: var(--text-mid);
    font-variant-numeric: tabular-nums;
  }
  .notnull {
    font-size: 0.7rem;
    color: var(--text-muted);
    font-style: italic;
  }
  .slot-badge { font-size: 0.8rem; color: var(--text-mid); font-variant-numeric: tabular-nums; }
  .expiry { font-size: 0.75rem; margin-left: 0.3rem; }
  .expiry-critical { color: var(--text-error); }
  .expiry-warn { color: var(--text-warn, #e6a817); }
  .loading { color: var(--text-muted); font-style: italic; }
  .error { color: var(--text-error); font-weight: bold; cursor: help; }
  .status-hint {
    display: block;
    font-size: 0.8rem;
    color: var(--text-muted);
    padding: 0.15rem 0.3rem;
    user-select: none;
  }
  .loading-hint,
  .empty-hint { font-style: italic; }
  .error-hint {
    color: var(--text-error);
    white-space: normal;
    word-break: break-word;
  }
  .children {
    padding-left: 1.2rem;
    border-left: 1px solid var(--border-light);
    margin-left: 0.4rem;
  }
</style>
