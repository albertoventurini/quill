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
      case "leaf": return `${kindTag(node.leafKind)} ${node.name}`;
    }
  }

  function arrow(): string {
    if (node.kind === "leaf") return "  ";
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
      <span class="label">{nodeLabel()}</span>
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
    {#if node.children === null}
      <!-- not yet loaded; loading spinner above handles feedback -->
    {:else if node.children.length === 0 && node.kind === "schema" && !node.loading}
      <div class="children">
        <span class="empty-hint">No tables or functions — right-click to Refresh schema</span>
      </div>
    {:else if node.children.length > 0}
      <div class="children">
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
      </div>
    {/if}
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
    text-align: center;
    color: var(--text-mid);
    font-size: 0.85rem;
  }
  .label { flex: 1; }
  .slot-badge { font-size: 0.8rem; color: var(--text-mid); font-variant-numeric: tabular-nums; }
  .expiry { font-size: 0.75rem; margin-left: 0.3rem; }
  .expiry-critical { color: var(--text-error); }
  .expiry-warn { color: var(--text-warn, #e6a817); }
  .loading { color: var(--text-muted); font-style: italic; }
  .error { color: var(--text-error); font-weight: bold; cursor: help; }
  .empty-hint {
    display: block;
    font-size: 0.8rem;
    color: var(--text-muted);
    font-style: italic;
    padding: 0.15rem 0.3rem;
    user-select: none;
  }
  .children {
    padding-left: 1.2rem;
    border-left: 1px solid var(--border-light);
    margin-left: 0.4rem;
  }
</style>
