<script lang="ts">
  import {
    api,
    type Connection,
    type NewConnection,
    type SlotState,
    type RunResult,
    type ChunkResult,
    type CommandError,
    type ColumnMeta,
  } from "$lib/tauri";
  import ResultGrid from "$lib/ResultGrid.svelte";
  import Editor from "$lib/Editor.svelte";
  import { statementAtCursor } from "$lib/statement";
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
  type ActiveResult = {
    resultId: string;
    columns: ColumnMeta[];
    rows: unknown[][];
    hasMore: boolean;
    rowCount: number;
    durationMs: number;
  };
  let active = $state<ActiveResult | null>(null);
  let lastError = $state<CommandError | null>(null);
  let loadingMore = $state(false);
  let editor = $state<Editor | undefined>(undefined);
  let editorWarning = $state<string | null>(null);

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
    if (active && selectedDb?.serverId === id) {
      active = null;
    }
    await api.disconnectServer(id);
    delete connectedState[id];
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

  async function selectDb(serverId: number, database: string) {
    if (
      active &&
      selectedDb &&
      (selectedDb.serverId !== serverId || selectedDb.database !== database)
    ) {
      await closeActive();
    }
    selectedDb = { serverId, database };
  }

  let selectedConn = $derived(
    selectedDb ? connections.find((c) => c.id === selectedDb!.serverId) ?? null : null,
  );

  // ═════════════════ Context menu ═════════════════

  function openMenu(e: MouseEvent, target: TreeNode) {
    e.preventDefault();
    e.stopPropagation();
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

  function buildPayloadFromButton() {
    // Without cursor info from the editor (button click), pretend we have
    // cursor at end and no selection.  The editor itself uses the real
    // selection on Cmd+Enter.
    return statementAtCursor(sql, sql.length, { from: sql.length, to: sql.length });
  }

  async function runFromEditor(payload: ReturnType<typeof statementAtCursor> | null) {
    if (!payload) {
      editorWarning = "Nothing to run — the buffer is empty.";
      return;
    }
    editorWarning = null;

    if (payload.multiStatement && !payload.isSelection) {
      editorWarning =
        "Multiple statements detected — running only the statement at the cursor (multi-statement scripts ship in v1.1).";
    }

    if (!selectedDb || !isConnected(selectedDb.serverId) || !payload.text.trim() || runningQuery) {
      return;
    }

    await closeActive();

    runningQuery = true;
    lastError = null;
    try {
      const r: RunResult = await api.runQuery(
        selectedDb.serverId,
        selectedDb.database,
        payload.text,
      );
      active = {
        resultId: r.result_id,
        columns: r.columns,
        rows: r.first_chunk,
        hasMore: r.has_more,
        rowCount: r.row_count_so_far,
        durationMs: r.duration_ms_so_far,
      };
    } catch (err) {
      lastError = err as CommandError;
    } finally {
      runningQuery = false;
    }
    if (selectedDb) await refreshSlotState(selectedDb.serverId);
  }

  async function loadMore() {
    if (!active || loadingMore) return;
    loadingMore = true;
    try {
      const chunk: ChunkResult = await api.fetchMore(active.resultId);
      active.rows = [...active.rows, ...chunk.rows];
      active.hasMore = chunk.has_more;
      active.rowCount = chunk.row_count_so_far;
      active.durationMs = chunk.duration_ms_so_far;
      if (!chunk.has_more) {
        active.resultId = "";
      }
    } catch (err) {
      lastError = err as CommandError;
      active = null;
    } finally {
      loadingMore = false;
    }
    if (selectedDb) await refreshSlotState(selectedDb.serverId);
  }

  async function closeActive() {
    if (!active) return;
    const rid = active.resultId;
    active = null;
    lastError = null;
    if (rid) {
      try {
        await api.closeResult(rid);
      } catch {
        // Best-effort; ignore.
      }
    }
    if (selectedDb) await refreshSlotState(selectedDb.serverId);
  }

  async function cancelRunning() {
    if (!selectedDb) return;
    try {
      await api.cancelQuery(selectedDb.serverId, selectedDb.database);
    } catch (err) {
      lastError = err as CommandError;
    }
    if (selectedDb) await refreshSlotState(selectedDb.serverId);
  }

  async function refreshSlotState(serverId: number) {
    const s = await api.getSlotState(serverId);
    if (s) connectedState[serverId] = s;
  }

  function statusLineText(a: ActiveResult): string {
    const slot = connectedState[selectedDb!.serverId];
    const busy = slot ? slot.slots.filter((s) => s.busy).length : 0;
    const budget = slot ? slot.budget : 0;
    const parts = [
      `${a.rowCount.toLocaleString()} rows`,
      `${a.durationMs}ms`,
      `slot [${busy}/${budget}]`,
      `${selectedConn?.name ?? "?"}@${selectedDb!.database}`,
      a.hasMore ? "cursor open" : "cursor closed",
    ];
    return parts.join(" · ");
  }

  let canRun = $derived(
    selectedDb !== null &&
      isConnected(selectedDb.serverId) &&
      sql.trim().length > 0 &&
      !runningQuery,
  );
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

      <Editor
        bind:this={editor}
        initial={sql}
        onChange={(doc) => { sql = doc; }}
        onRun={(payload) => runFromEditor(payload)}
      />

      <div class="action-row">
        <button class="btn" onclick={() => runFromEditor(buildPayloadFromButton())} disabled={!canRun}>
          {runningQuery ? "Running…" : "Run (Ctrl/Cmd+Enter)"}
        </button>

        <button
          class="btn"
          onclick={cancelRunning}
          disabled={!runningQuery && !active}
        >Cancel</button>

        {#if active}
          <button class="btn" onclick={closeActive}>Close result</button>
        {/if}
      </div>

      {#if editorWarning}
        <p class="muted inline">{editorWarning}</p>
      {/if}
      {#if lastError}
        <p class="inline error">
          <span class="err-badge">{lastError.kind}</span>
          {lastError.message}
        </p>
      {/if}

      {#if active}
        <ResultGrid
          columns={active.columns}
          rows={active.rows}
          statusLine={statusLineText(active)}
          hasMore={active.hasMore}
          loadingMore={loadingMore}
          onLoadMore={loadMore}
          canLoadMore={!!active.resultId}
        />
      {:else if !runningQuery && !lastError}
        <p class="muted">No active result. Press Run or Ctrl/Cmd+Enter.</p>
      {/if}

      {#if !isConnected(sd.serverId)}
        <p class="muted">Not connected. Right-click the server in the tree → Connect.</p>
      {/if}
    {:else}
      <p class="muted">Select a database in the tree (left) to start querying.</p>
    {/if}
  </main>
</div>

<!-- ═══════ CONTEXT MENU ═══════ -->
{#if menu}
  {@const items = menuItemsFor(menu.target)}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions a11y_click_events_have_key_events -->
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
      <!-- svelte-ignore a11y_autofocus -->
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

  .action-row { display: flex; gap: 0.5rem; align-items: center; }
  .inline { margin: 0.25rem 0; }
  .inline.error { color: #b00020; font-size: 0.9rem; }
  .err-badge {
    display: inline-block;
    padding: 0 0.35rem;
    margin-right: 0.4rem;
    border-radius: 3px;
    background: #b00020;
    color: white;
    font-size: 0.75rem;
    text-transform: uppercase;
  }
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
