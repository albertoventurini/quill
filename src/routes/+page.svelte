<script lang="ts">
  import {
    api,
    type Connection,
    type NewConnection,
    type SlotState,
    type RunResult,
    type ChunkResult,
    type CommandError,
  } from "$lib/tauri";
  import ResultGrid from "$lib/ResultGrid.svelte";
  import Editor from "$lib/Editor.svelte";
  import { statementAtCursor } from "$lib/statement";
  import Tree from "$lib/Tree.svelte";
  import type { ServerNode, TreeNode, DatabaseNode } from "$lib/tree";
  import { clearDatabaseSubtree, errorMessage, loadDatabases } from "$lib/tree";
  import { clearServerSchemaPayloads } from "$lib/schemaStore";
import Tabs from "$lib/Tabs.svelte";
import { makeTab, type Tab } from "$lib/tabs";
import SidePanel from "$lib/SidePanel.svelte";
import SaveDialog from "$lib/SaveDialog.svelte";
import Resizer from "$lib/Resizer.svelte";
import { save } from "@tauri-apps/plugin-dialog";
  import { encodeCsv } from "$lib/csv";
  import { getStoredTheme, setTheme, type Theme } from "$lib/theme.svelte";

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

  let editingId = $state<number | null>(null);

  // Settings dialog
  let settingsDialog = $state<HTMLDialogElement | null>(null);
  let baoAddr = $state("");
  let baoOidcRole = $state("");
  let baoHasToken = $state(false);
  let baoTokenPersisted = $state(false);
  let baoStatusError = $state("");
  let baoLoginBusy = $state(false);

  let expiryPollHandle = $state<ReturnType<typeof setInterval> | null>(null);

  // Context menu (new).
  let menu = $state<{
    x: number;
    y: number;
    target: TreeNode;
  } | null>(null);

  // Tab state (M5.3).
  let tabs = $state<Tab[]>([]);
  let activeTabId = $state<number | null>(null);

  let activeTab = $derived(
    activeTabId === null ? null : tabs.find((t) => t.id === activeTabId) ?? null,
  );

  let editor = $state<Editor | undefined>(undefined);

  // Change-database dialog state
  let dbDialog = $state<HTMLDialogElement | null>(null);
  let dbDialogTabId = $state<number | null>(null);
  let dbDialogPick = $state<string>("");
  let dbDialogOptions = $state<string[]>([]);
  let dbDialogError = $state<string>("");

  // Side panel + save state
  let sidePanelError = $state<string | null>(null);
  let saveDialog = $state<SaveDialog | undefined>(undefined);
  let savedListRefreshKey = $state(0);

  let leftPaneWidth = $state(320);
  let editorHeight = $state(220);
  let sidePanelHeight = $state(240);

  function resizeLeftPane(delta: number) {
    leftPaneWidth = Math.max(180, Math.min(window.innerWidth * 0.5, leftPaneWidth + delta));
  }

  function resizeEditor(delta: number) {
    editorHeight = Math.max(160, editorHeight + delta);
  }

  function resizeSidePanel(delta: number) {
    sidePanelHeight = Math.max(120, sidePanelHeight - delta);
  }

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
      credential_source: "password",
      bao_role_path: null,
    };
  }

  function expiryRemainingMs(state: SlotState | undefined): number {
    const e = state?.credential_expiry;
    if (!e) return Infinity;
    return e.secs_since_epoch * 1000 + Math.floor(e.nanos_since_epoch / 1_000_000) - Date.now();
  }

  // ═════════════════ Helpers ═════════════════

  function isConnected(id: number): boolean {
    return id in connectedState;
  }

  function slotLabel(s: SlotState | undefined): string {
    if (!s) return "";
    const busy = s.slots.filter((x) => x.busy).length;
    return `(${s.budget - busy}/${s.budget})`;
  }

  // Slot usage for the budget-full notice, derived from the state already
  // fetched after every query.
  function budgetInfo(tab: Tab): { busy: number; budget: number } {
    const s = connectedState[tab.serverId];
    return {
      busy: s ? s.slots.filter((x) => x.busy).length : 0,
      budget: s ? s.budget : 0,
    };
  }

  // Which other tabs hold a slot on this server, and why.  Every busy slot is
  // held either by an in-flight query (`runningQuery`) or by an open result
  // cursor (`active.resultId`) — a `CancelRequest` only frees the former, so
  // the notice must offer the matching remedy for each.
  function slotHolders(tab: Tab): { running: Tab[]; results: Tab[] } {
    const others = tabs.filter((t) => t.serverId === tab.serverId && t.id !== tab.id);
    return {
      running: others.filter((t) => t.runningQuery),
      results: others.filter((t) => !!t.active?.resultId),
    };
  }

  function heldByText(tab: Tab): string {
    const h = slotHolders(tab);
    const parts = [
      ...h.results.map((t) => `${t.database} — result open`),
      ...h.running.map((t) => `${t.database} — query running`),
    ];
    return parts.join("; ");
  }

  // Cancel in-flight queries on this server.  Cancellation is out-of-band, so
  // the slot frees only once the cancelled query's guard drops — poll briefly
  // so the notice reflects the freed slot.
  async function cancelHeldQuery(tab: Tab) {
    try {
      await api.cancelQuery(tab.serverId, null);
    } catch (err) {
      tab.lastError = err as CommandError;
    }
    for (let i = 0; i < 20; i++) {
      await refreshSlotState(tab.serverId);
      const s = connectedState[tab.serverId];
      if (s && s.slots.filter((x) => x.busy).length < s.budget) break;
      await new Promise((r) => setTimeout(r, 100));
    }
  }

  function retryQuery(tab: Tab) {
    tab.lastError = null;
    void runFromEditor(buildPayloadFromButton(tab));
  }

  function raiseBudget(tab: Tab) {
    const conn = connections.find((c) => c.id === tab.serverId);
    if (conn) openEditModal(conn);
  }

  // ═════════════════ Add connection (unchanged shape) ═════════════════

  function openAddModal() {
    editingId = null;
    addForm = defaultAddForm();
    addError = "";
    addDialog?.showModal();
  }

  function openEditModal(conn: Connection) {
    editingId = conn.id;
    addForm = {
      name: conn.name,
      host: conn.host,
      port: conn.port,
      default_db: conn.default_db,
      username: conn.username,
      ssl_mode: conn.ssl_mode,
      slot_budget: conn.slot_budget,
      password_ref: null,
      credential_source: conn.credential_source,
      bao_role_path: conn.bao_role_path,
    };
    addError = "";
    addDialog?.showModal();
  }

  async function saveConnection(e: Event) {
    e.preventDefault();
    addError = "";
    try {
      if (editingId !== null) {
        await api.updateConnection(editingId, { ...addForm });
      } else {
        await api.saveConnection({ ...addForm });
      }
      const rows = await api.listConnections();
      connections = rows;
      tree = rows.map(makeServerNode);
      addDialog?.close();
      editingId = null;
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
        const serverNode = tree.find((n) => n.conn.id === pwTargetId);
        if (serverNode) {
          serverNode.expanded = true;
          loadDatabases(serverNode);
        }
        pwDialog?.close();
    } catch (err) {
      pwError = errorMessage(err);
    }
  }

  async function disconnect(id: number) {
    // Close any tabs targeting this server, freeing their results first.
    const targets = tabs.filter((t) => t.serverId === id);
    for (const t of targets) {
      if (t.active?.resultId) {
        try { await api.closeResult(t.active.resultId); } catch {}
      }
    }
    tabs = tabs.filter((t) => t.serverId !== id);
    if (activeTabId !== null && !tabs.some((t) => t.id === activeTabId)) {
      activeTabId = tabs.length ? tabs[tabs.length - 1].id : null;
    }

    await api.disconnectServer(id);
    clearServerSchemaPayloads(id);
    delete connectedState[id];
    if (editingId === id) editingId = null;
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

  // ═════════════════ OpenBao Settings ═════════════════

  async function refreshOpenBaoStatus() {
    try {
      const settings = await api.getAllSettings();
      baoAddr = settings["openbao_addr"] ?? "";
      // "default" was a bad shipped default, not a real role name; treat it as blank, which
      // tells OpenBao to use the mount's configured default role.
      const storedRole = settings["openbao_oidc_role"] ?? "";
      baoOidcRole = storedRole === "default" ? "" : storedRole;
      const status = await api.openBaoTokenStatus();
      baoHasToken = status.present;
      baoTokenPersisted = status.persisted;
      baoStatusError = "";
    } catch (err) {
      baoStatusError = errorMessage(err);
    }
  }

  async function saveBaoSettings() {
    baoStatusError = "";
    try {
      await api.setSetting("openbao_addr", baoAddr);
      await api.setSetting("openbao_oidc_role", baoOidcRole);
      await refreshOpenBaoStatus();
    } catch (err) {
      baoStatusError = errorMessage(err);
    }
  }

  async function loginOpenBao() {
    baoStatusError = "";
    baoLoginBusy = true;
    try {
      const { persisted } = await api.loginOpenBao();
      await refreshOpenBaoStatus();
      baoStatusError = persisted
        ? "Login successful."
        : "Login successful, but no OS keyring is available — the token is kept in memory only and you'll need to log in again after restarting Quill.";
    } catch (err) {
      baoStatusError = errorMessage(err);
    } finally {
      baoLoginBusy = false;
    }
  }

  async function clearOpenBaoToken() {
    baoStatusError = "";
    try {
      await api.clearOpenBaoToken();
      await refreshOpenBaoStatus();
    } catch (err) {
      baoStatusError = errorMessage(err);
    }
  }

  function openSettings() {
    refreshOpenBaoStatus();
    settingsDialog?.showModal();
  }

  $effect(() => {
    const connected = Object.keys(connectedState).map(Number);
    if (connected.length === 0) {
      if (expiryPollHandle) { clearInterval(expiryPollHandle); expiryPollHandle = null; }
      return;
    }
    if (expiryPollHandle) return;
    expiryPollHandle = setInterval(async () => {
      for (const sid of connected) {
        try {
          const s = await api.getSlotState(sid);
          if (s) connectedState[sid] = s;
        } catch {}
      }
    }, 30_000);
  });

  // ═════════════════ Connect routing ═════════════════

  function connectServer(id: number) {
    const conn = connections.find((c) => c.id === id);
    if (!conn) return;
    if (conn.credential_source === "openbao") {
      connectViaOpenBao(id);
    } else {
      promptPassword(id);
    }
  }

  async function connectViaOpenBao(id: number) {
    const serverNode = tree.find((n) => n.conn.id === id);
    if (!serverNode) return;
    serverNode.loading = true;
    serverNode.error = null;
    try {
      const state = await api.connectServer(id, null);
      connectedState[id] = state;
      serverNode.expanded = true;
      loadDatabases(serverNode);
    } catch (err) {
      serverNode.error = errorMessage(err);
    } finally {
      serverNode.loading = false;
    }
  }

  async function refreshOpenBaoCreds(id: number) {
    try {
      const state = await api.refreshOpenBaoCreds(id);
      connectedState[id] = state;
    } catch (err) {
      console.error("refresh failed", err);
    }
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

  // Clicking a tree node only moves the selection cursor (side-panel scope,
  // tab "matches" styling, and the target for the "+" / "New query" actions).
  // It never opens an editor — that is an explicit context-menu action.
  function selectDb(serverId: number, database: string) {
    selectedDb = { serverId, database };
  }

  /** Open a fresh editor tab targeting `(serverId, database)`, optionally
   *  scoped to `schema`.  Invoked by the tree context menu. */
  function openQueryEditor(serverId: number, database: string, schema: string | null) {
    if (!connections.some((c) => c.id === serverId)) {
      sidePanelError = "This connection has been deleted.";
      return;
    }
    selectedDb = { serverId, database };
    const tab = makeTab(serverId, database, "", schema);
    tabs.push(tab);
    activeTabId = tab.id;
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
      case "open-query":
        if (target.kind === "database") {
          openQueryEditor(target.serverId, target.name, null);
        } else if (target.kind === "schema") {
          openQueryEditor(target.serverId, target.database, target.name);
        }
        break;
      case "connect":
        if (target.kind === "server") connectServer(target.conn.id);
        break;
      case "edit":
        if (target.kind === "server") openEditModal(target.conn);
        break;
      case "disconnect":
        if (target.kind === "server") await disconnect(target.conn.id);
        break;
      case "refresh-openbao":
        if (target.kind === "server") await refreshOpenBaoCreds(target.conn.id);
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
      case "column": return t.name;
    }
  }

  // What menu items apply to this target?
  function menuItemsFor(t: TreeNode): { action: string; label: string }[] {
    const items: { action: string; label: string }[] = [];
    if (t.kind === "server") {
      if (isConnected(t.conn.id)) {
        items.push({ action: "disconnect", label: "Disconnect" });
        if (t.conn.credential_source === "openbao") {
          items.push({ action: "refresh-openbao", label: "Refresh OpenBao credentials" });
        }
      } else {
        items.push({ action: "connect", label: "Connect…" });
        items.push({ action: "edit", label: "Edit connection…" });
      }
      items.push({ action: "copy-name", label: "Copy name" });
      if (!isConnected(t.conn.id)) {
        items.push({ action: "delete", label: "Delete connection" });
      }
    } else if (t.kind === "database") {
      if (isConnected(t.serverId)) {
        items.push({ action: "open-query", label: "New query" });
      }
      items.push({ action: "refresh", label: "Refresh schema" });
      items.push({ action: "copy-name", label: "Copy name" });
    } else if (t.kind === "schema") {
      if (isConnected(t.serverId)) {
        items.push({ action: "open-query", label: "New query scoped to this schema" });
      }
      items.push({ action: "refresh", label: "Refresh schema" });
      items.push({ action: "copy-name", label: "Copy qualified name" });
    } else if (t.kind === "leaf") {
      items.push({ action: "refresh", label: "Refresh schema" });
      items.push({ action: "copy-name", label: "Copy qualified name" });
    } else if (t.kind === "column") {
      items.push({ action: "copy-name", label: "Copy column name" });
    } else if (t.kind === "group") {
      items.push({ action: "refresh", label: "Refresh schema" });
    }
    return items;
  }

  // ═════════════════ Tab lifecycle ═════════════════

  // "+" opens a tab equivalent to the active one: same server, database, and
  // schema scope.  Falls back to the tree selection only when no tab is open.
  function addTab() {
    if (activeTab) {
      const tab = makeTab(activeTab.serverId, activeTab.database, "", activeTab.schema);
      tabs.push(tab);
      activeTabId = tab.id;
      return;
    }
    if (!selectedDb) return;
    const tab = makeTab(selectedDb.serverId, selectedDb.database, "");
    tabs.push(tab);
    activeTabId = tab.id;
  }

  function defaultDbForServer(serverId: number): string {
    return connections.find((c) => c.id === serverId)?.default_db ?? "postgres";
  }

  function openInNewTab(
    serverId: number,
    database: string,
    sql: string,
    schema: string | null = null,
  ) {
    if (!connections.some((c) => c.id === serverId)) {
      sidePanelError = "This connection has been deleted.";
      return;
    }
    const tab = makeTab(serverId, database, sql, schema);
    tabs.push(tab);
    activeTabId = tab.id;
  }

  function openSaveDialog() {
    if (!activeTab) return;
    saveDialog?.open();
  }

  async function closeTab(id: number) {
    const tab = tabs.find((t) => t.id === id);
    if (!tab) return;

    // Stop in-flight work so the backend doesn't leak a slot.
    if (tab.runningQuery) {
      try {
        await api.cancelQuery(tab.serverId, tab.database);
      } catch { /* best-effort */ }
    }
    if (tab.active?.resultId) {
      try {
        await api.closeResult(tab.active.resultId);
      } catch { /* best-effort */ }
    }

    tabs = tabs.filter((t) => t.id !== id);

    // Refocus: prefer the tab to the right; fall back to the one to the left.
    if (activeTabId === id) {
      const remaining = tabs;
      activeTabId = remaining.length ? remaining[Math.max(0, remaining.length - 1)].id : null;
    }

    // Slot may have freed.
    await refreshSlotState(tab.serverId);
  }

  function selectTab(id: number) {
    activeTabId = id;
  }

  async function openChangeDbDialog(tabId: number) {
    const tab = tabs.find((t) => t.id === tabId);
    if (!tab) return;
    if (!isConnected(tab.serverId)) {
      // Can't list DBs without an active connection.
      tab.lastError = {
        kind: "NotConnected",
        message: "Connect to the server first to change databases.",
      };
      return;
    }
    dbDialogTabId = tabId;
    dbDialogError = "";
    dbDialogPick = tab.database;
    try {
      const dbs = await api.listDatabases(tab.serverId);
      dbDialogOptions = dbs.map((d) => d.name);
    } catch (err) {
      dbDialogOptions = [];
      dbDialogError = errorMessage(err);
    }
    dbDialog?.showModal();
  }

  async function submitChangeDb(e: Event) {
    e.preventDefault();
    if (dbDialogTabId === null) return;
    const tab = tabs.find((t) => t.id === dbDialogTabId);
    if (!tab) return;

    // Close any active result on the old DB first — same hygiene rule as
    // M3.6's "switching DBs closes the prior result," but applied at the
    // tab level.
    if (tab.active?.resultId) {
      try { await api.closeResult(tab.active.resultId); } catch {}
      tab.active = null;
    }

    tab.database = dbDialogPick;
    // Database changed → previous SQL may not parse against the new schema,
    // but the buffer is the user's; don't clear it.
    dbDialog?.close();
    await refreshSlotState(tab.serverId);
  }

  // ═════════════════ Query ═════════════════

  function buildPayloadFromButton(tab: Tab) {
    // Without cursor info from the editor (button click), pretend cursor at end.
    return statementAtCursor(tab.sql, tab.sql.length, {
      from: tab.sql.length,
      to: tab.sql.length,
    });
  }

  function canRun(tab: Tab): boolean {
    return isConnected(tab.serverId) && tab.sql.trim().length > 0 && !tab.runningQuery;
  }

  async function runFromEditor(payload: ReturnType<typeof statementAtCursor> | null) {
    const tab = activeTab;
    if (!tab) return;

    if (!payload) {
      tab.editorWarning = "Nothing to run — the buffer is empty.";
      return;
    }
    tab.editorWarning = null;

    if (payload.multiStatement && !payload.isSelection) {
      tab.editorWarning =
        "Multiple statements detected — running only the statement at the cursor (multi-statement scripts ship in v1.1).";
    }

    if (!isConnected(tab.serverId) || !payload.text.trim() || tab.runningQuery) {
      return;
    }

    // Close any prior result on this tab.
    await closeActive(tab);

    tab.runningQuery = true;
    tab.lastError = null;
    try {
      const r: RunResult = await api.runQuery(tab.serverId, tab.database, payload.text, tab.schema);
      tab.active = {
        resultId: r.result_id,
        columns: r.columns,
        rows: r.first_chunk,
        hasMore: r.has_more,
        rowCount: r.row_count_so_far,
        durationMs: r.duration_ms_so_far,
      };
      if (!r.has_more) tab.active.resultId = "";
    } catch (err) {
      tab.lastError = err as CommandError;
    } finally {
      tab.runningQuery = false;
    }
    await refreshSlotState(tab.serverId);
  }

  async function loadMore() {
    const tab = activeTab;
    if (!tab || !tab.active || tab.loadingMore) return;
    tab.loadingMore = true;
    try {
      const chunk: ChunkResult = await api.fetchMore(tab.active.resultId);
      tab.active.rows = [...tab.active.rows, ...chunk.rows];
      tab.active.hasMore = chunk.has_more;
      tab.active.rowCount = chunk.row_count_so_far;
      tab.active.durationMs = chunk.duration_ms_so_far;
      if (!chunk.has_more) tab.active.resultId = "";
    } catch (err) {
      tab.lastError = err as CommandError;
      tab.active = null;
    } finally {
      tab.loadingMore = false;
    }
    await refreshSlotState(tab.serverId);
  }

  async function closeActive(tab: Tab | null = activeTab) {
    if (!tab || !tab.active) return;
    const rid = tab.active.resultId;
    tab.active = null;
    tab.lastError = null;
    if (rid) {
      try { await api.closeResult(rid); } catch {}
    }
    await refreshSlotState(tab.serverId);
  }

  async function cancelRunning() {
    const tab = activeTab;
    if (!tab) return;
    try {
      await api.cancelQuery(tab.serverId, tab.database);
    } catch (err) {
      tab.lastError = err as CommandError;
    }
    await refreshSlotState(tab.serverId);
  }

  async function refreshSlotState(serverId: number) {
    const s = await api.getSlotState(serverId);
    if (s) connectedState[serverId] = s;
  }

  function statusLineText(tab: Tab): string {
    const a = tab.active!;
    const slot = connectedState[tab.serverId];
    const busy = slot ? slot.slots.filter((s) => s.busy).length : 0;
    const budget = slot ? slot.budget : 0;
    const serverName = connections.find((c) => c.id === tab.serverId)?.name ?? "?";
    const parts = [
      `${a.rowCount.toLocaleString()} rows`,
      `${a.durationMs}ms`,
      `slot (${budget - busy}/${budget})`,
      `${serverName}@${tab.database}`,
      a.hasMore ? "cursor open" : "cursor closed",
    ];
    return parts.join(" · ");
  }

  function csvFilename(tab: Tab): string {
    const server = connections.find((c) => c.id === tab.serverId)?.name ?? "server";
    const safe = (s: string) => s.replace(/[^a-zA-Z0-9._-]+/g, "_");
    const now = new Date();
    const stamp =
      `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}` +
      `-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
    const partial = tab.active?.hasMore ? ".partial" : "";
    return `${safe(server)}-${safe(tab.database)}-${stamp}${partial}.csv`;
  }
  function pad(n: number): string {
    return n < 10 ? `0${n}` : `${n}`;
  }

  async function exportCsv(tab: Tab, sortedRows: unknown[][]) {
    if (!tab.active) return;
    const filename = csvFilename(tab);
    let target: string | null;
    try {
      target = await save({
        defaultPath: filename,
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
    } catch (err) {
      tab.lastError = err as CommandError;
      return;
    }
    if (!target) return;

    const prelude =
      tab.active.hasMore
        ? `# Partial export: ${tab.active.rowCount} rows, cursor still open. -- generated by Quill`
        : undefined;
    const payload = encodeCsv(tab.active.columns, sortedRows, prelude);

    try {
      await api.writeTextFile(target, payload);
    } catch (err) {
      tab.lastError = err as CommandError;
    }
  }

  async function copyCsv(tab: Tab, sortedRows: unknown[][]) {
    if (!tab.active) return;
    const prelude = tab.active.hasMore
      ? `# Partial export: ${tab.active.rowCount} rows, cursor still open. -- generated by Quill`
      : undefined;
    const payload = encodeCsv(tab.active.columns, sortedRows, prelude);
    try {
      await navigator.clipboard.writeText(payload);
    } catch (err) {
      tab.lastError = {
        kind: "Pg",
        message: `clipboard write failed: ${err}`,
      };
    }
  }
</script>

<svelte:window onclick={closeMenu} oncontextmenu={(e) => { if (!menu) return; e.preventDefault(); closeMenu(); }} />

<div class="shell">
  <!-- ═══════ LEFT PANE ═══════ -->
  <aside class="left-pane" style="width: {leftPaneWidth}px">
    <div class="header-row">
      <h2>Connections</h2>
      <div style="display: flex; gap: 0.3rem;">
        <button class="btn" onclick={openSettings} title="Settings">⚙</button>
        <button class="btn" onclick={openAddModal}>+ Add</button>
      </div>
    </div>

    {#if tree.length === 0}
      <p class="muted">No saved connections.</p>
    {:else}
      <div class="tree">
        {#each tree as serverNode (serverNode.conn.id)}
              <Tree
                node={serverNode}
                {isConnected}
                {selectedDb}
                onSelectDb={selectDb}
                onContextMenu={openMenu}
                onConnectServer={connectServer}
                slotLabel={slotLabel(connectedState[serverNode.conn.id])}
                expiryRemainingMs={expiryRemainingMs(connectedState[serverNode.conn.id])}
              />
            {/each}
      </div>
    {/if}

    <Resizer orientation="vertical" onResize={resizeSidePanel} />

    <SidePanel
      height={sidePanelHeight}
      selectedServerId={selectedDb?.serverId ?? null}
      resolveDb={defaultDbForServer}
      onOpenInNewTab={openInNewTab}
      onError={(msg) => (sidePanelError = msg)}
    />
    {#if sidePanelError}
      <p class="inline error" style="padding: 0.25rem 0.5rem;">
        <span class="err-badge">side panel</span>
        {sidePanelError}
        <button class="btn" style="margin-left: 0.4rem; padding: 0 0.4rem;" onclick={() => (sidePanelError = null)}>×</button>
      </p>
    {/if}
  </aside>

  <Resizer orientation="horizontal" onResize={resizeLeftPane} />

  <!-- ═══════ RIGHT PANE ═══════ -->
  <main class="right-pane">
    {#if tabs.length > 0}
      <Tabs
        {tabs}
        activeId={activeTabId}
        treeServerId={selectedDb?.serverId ?? null}
        treeDatabase={selectedDb?.database ?? null}
        serverNameLookup={(id) => connections.find((c) => c.id === id)?.name ?? "?"}
        onSelect={selectTab}
        onClose={closeTab}
        onAdd={addTab}
        onChangeDatabase={openChangeDbDialog}
      />
    {/if}

    {#if activeTab}
      {@const tab = activeTab}
      {#key tab.id}
        <Editor
          bind:this={editor}
          height={editorHeight}
          initial={tab.sql}
          onChange={(doc) => { tab.sql = doc; }}
          onRun={(payload) => runFromEditor(payload)}
          getContext={() => ({ serverId: tab.serverId, database: tab.database, schema: tab.schema })}
        />
      {/key}

      <div class="action-row">
        <button class="btn" onclick={() => runFromEditor(buildPayloadFromButton(tab))} disabled={!canRun(tab)}>
          {tab.runningQuery ? "Running…" : "Run (Ctrl/Cmd+Enter)"}
        </button>
        <button class="btn" onclick={cancelRunning} disabled={!tab.runningQuery && !tab.active}>
          Cancel
        </button>
        {#if tab.active}
          <button class="btn" onclick={() => closeActive(tab)}>Close result</button>
        {/if}
        <button class="btn" onclick={openSaveDialog} disabled={!tab.sql.trim()}>Save…</button>
      </div>

      {#if tab.editorWarning}
        <p class="muted inline">{tab.editorWarning}</p>
      {/if}
      {#if tab.lastError?.kind === "BudgetFull"}
        {@const info = budgetInfo(tab)}
        {@const held = slotHolders(tab)}
        <div class="inline notice budget-full">
          <p class="notice-head">
            Connection budget full ({info.busy} / {info.budget} in use)
          </p>
          {#if held.results.length || held.running.length}
            <p class="notice-sub">Held by: {heldByText(tab)}</p>
          {/if}
          <div class="notice-actions">
            {#if held.running.length}
              <button class="btn" onclick={() => cancelHeldQuery(tab)}>
                {held.running.length > 1 ? "Cancel running queries" : "Cancel running query"}
              </button>
            {/if}
            <button class="btn" onclick={() => retryQuery(tab)}>Retry</button>
          </div>
          <p class="notice-sub">
            Or <button class="linklike" onclick={() => raiseBudget(tab)}>raise the slot budget</button>
            for this connection.
          </p>
        </div>
      {:else if tab.lastError}
        <p class="inline error">
          <span class="err-badge">{tab.lastError.kind}</span>
          {tab.lastError.message}
        </p>
      {/if}

      <Resizer orientation="vertical" onResize={resizeEditor} />

      {#if tab.active}
        <ResultGrid
          columns={tab.active.columns}
          rows={tab.active.rows}
          statusLine={statusLineText(tab)}
          hasMore={tab.active.hasMore}
          loadingMore={tab.loadingMore}
          onLoadMore={loadMore}
          canLoadMore={!!tab.active.resultId}
          onExportCsv={(sorted) => exportCsv(tab, sorted)}
          onCopyCsv={(sorted) => copyCsv(tab, sorted)}
        />
      {:else if !tab.runningQuery && !tab.lastError}
        <p class="muted">No active result. Press Run or Ctrl/Cmd+Enter.</p>
      {/if}

      {#if !isConnected(tab.serverId)}
        <p class="muted">Not connected. Right-click the server in the tree → Connect.</p>
      {/if}

      <SaveDialog
        bind:this={saveDialog}
        initialSql={tab.sql}
        serverIdHint={tab.serverId}
        onSaved={() => { savedListRefreshKey += 1; }}
        onError={(msg) => (sidePanelError = msg)}
      />
    {:else}
      <p class="muted">Right-click a database (or a schema, to scope it) in the tree → “New query” to open an editor.</p>
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
  <h2>{editingId !== null ? "Edit Connection" : "Add Connection"}</h2>
  <form onsubmit={saveConnection} class="add-form">
    <label class="field">Name<input class="input" bind:value={addForm.name} required /></label>
    <label class="field">Host<input class="input" bind:value={addForm.host} required /></label>
    <label class="field">Port<input class="input" type="number" min={1} max={65535} bind:value={addForm.port} /></label>
    <label class="field">Default database<input class="input" bind:value={addForm.default_db} required /></label>
    <label class="field">Username<input class="input" bind:value={addForm.username} required /></label>
    <label class="field">
      Credential source
      <select class="input" bind:value={addForm.credential_source}>
        <option value="password">Password</option>
        <option value="openbao">OpenBao</option>
      </select>
    </label>
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

    <div class="field">
      Mode
      <label class="radio"><input type="radio" name="conn-mode" checked disabled /> Read-only</label>
      <label class="radio"><input type="radio" name="conn-mode" disabled /> Read-write</label>
      <span class="hint">Read-write isn’t supported yet — all queries run in a read-only transaction.</span>
    </div>

    {#if addForm.credential_source === "openbao"}
      <label class="field">
        Role path
        <input class="input" bind:value={addForm.bao_role_path}
               placeholder="database/creds/my-role" />
      </label>
    {/if}

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

<!-- ═══════ SETTINGS DIALOG ═══════ -->
<dialog bind:this={settingsDialog} class="modal">
  <h2>Settings</h2>
  <div class="add-form">
    <h3>Theme</h3>
    <div class="theme-options">
      <label class="theme-option">
        <input type="radio" name="theme" value="light" checked={getStoredTheme() === "light"} onchange={() => setTheme("light")} />
        Light
      </label>
      <label class="theme-option">
        <input type="radio" name="theme" value="dark" checked={getStoredTheme() === "dark"} onchange={() => setTheme("dark")} />
        Dark
      </label>
      <label class="theme-option">
        <input type="radio" name="theme" value="system" checked={getStoredTheme() === "system"} onchange={() => setTheme("system")} />
        System
      </label>
    </div>

    <h3>OpenBao</h3>
    <label class="field">
      Server address
      <input class="input"
             bind:value={baoAddr}
             placeholder="https://vault.internal:8200" />
    </label>
    <label class="field">
      OIDC role <span style="opacity: 0.6;">(blank = server default)</span>
      <input class="input"
             bind:value={baoOidcRole}
             placeholder="leave blank for default role" />
    </label>
    <button class="btn btn-primary" onclick={saveBaoSettings}>Save settings</button>

    <p style="margin-top: 1rem;">
      Token: <strong>{baoHasToken ? "Present" : "None"}</strong>
      {#if baoHasToken && !baoTokenPersisted}
        <span style="opacity: 0.7;">(in memory only — re-login needed after restart)</span>
      {/if}
    </p>
    <div class="modal-actions" style="justify-content: flex-start;">
      <button class="btn btn-primary" onclick={loginOpenBao} disabled={baoLoginBusy}>
        {baoLoginBusy ? "Opening browser…" : "Login with browser"}
      </button>
      {#if baoHasToken}
        <button class="btn" onclick={clearOpenBaoToken}>Clear token</button>
      {/if}
    </div>

    {#if baoStatusError}
      <p class="error">{baoStatusError}</p>
    {/if}

    <div class="modal-actions" style="margin-top: 1rem;">
      <button type="button" class="btn" onclick={() => settingsDialog?.close()}>Close</button>
    </div>
  </div>
</dialog>

<!-- ═══════ CHANGE-DATABASE DIALOG ═══════ -->
<dialog bind:this={dbDialog} class="modal">
  <h2>Change database</h2>
  <form onsubmit={submitChangeDb} class="add-form">
    {#if dbDialogError}
      <p class="error">{dbDialogError}</p>
    {:else}
      <label class="field">
        Database
        <select class="input" bind:value={dbDialogPick}>
          {#each dbDialogOptions as db}
            <option value={db}>{db}</option>
          {/each}
        </select>
      </label>
      <p class="muted" style="font-size: 0.85rem;">
        Closes the tab's current result, if any.
      </p>
    {/if}
    <div class="modal-actions">
      <button type="button" class="btn" onclick={() => dbDialog?.close()}>Cancel</button>
      <button type="submit" class="btn btn-primary" disabled={!!dbDialogError}>Change</button>
    </div>
  </form>
</dialog>

<style>
  :global(html), :global(body) { margin: 0; padding: 0; overflow: hidden; background: var(--bg-app); color: var(--text-primary); }
  .shell { display: flex; flex-direction: row; height: 100vh; overflow: hidden; background: var(--bg-app); }
  .left-pane { border-right: 1px solid var(--border-primary); padding: 0.75rem; overflow-y: auto; display: flex; flex-direction: column; gap: 0.5rem; }
  .right-pane { flex: 1; min-width: 0; min-height: 0; padding: 1rem; overflow: hidden; display: flex; flex-direction: column; gap: 0.5rem; }

  .header-row { display: flex; align-items: center; justify-content: space-between; }
  h2, h3 { margin: 0; font-size: 1.05rem; }

  .tree { flex: 1; overflow-y: auto; display: flex; flex-direction: column; }

  .btn { padding: 0.3rem 0.6rem; border: 1px solid var(--btn-border); border-radius: 4px; background: var(--btn-bg); cursor: pointer; font: inherit; font-size: 0.9rem; }
  .btn:hover { background: var(--btn-hover); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-primary { background: var(--btn-primary-bg); color: var(--btn-primary-text); border-color: var(--btn-primary-border); }
  .btn-primary:hover { background: var(--btn-primary-hover); }

  .input { padding: 0.35rem; border: 1px solid var(--border-input); border-radius: 4px; font: inherit; box-sizing: border-box; background: var(--bg-surface); color: var(--text-primary); }

  .action-row { display: flex; gap: 0.5rem; align-items: center; }
  .inline { margin: 0.25rem 0; }
  .inline.error { color: var(--text-error); font-size: 0.9rem; }
  .err-badge {
    display: inline-block;
    padding: 0 0.35rem;
    margin-right: 0.4rem;
    border-radius: 3px;
    background: var(--err-badge-bg);
    color: var(--err-badge-text);
    font-size: 0.75rem;
    text-transform: uppercase;
  }
  .error { color: var(--text-error); }

  .inline.notice.budget-full {
    border: 1px solid var(--text-warning);
    border-radius: 4px;
    padding: 0.5rem 0.65rem;
    font-size: 0.9rem;
  }
  .notice-head { margin: 0 0 0.2rem; font-weight: 600; color: var(--text-warning); }
  .notice-sub { margin: 0.2rem 0 0; color: var(--text-muted); }
  .notice-actions { display: flex; gap: 0.5rem; margin-top: 0.4rem; }
  .linklike {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: var(--text-warning);
    text-decoration: underline;
    cursor: pointer;
  }

  .modal { border: 1px solid var(--border-secondary); border-radius: 8px; padding: 1.25rem; max-width: 400px; width: 90%; background: var(--bg-surface); }
  .modal::backdrop { background: var(--modal-backdrop); }
  .add-form { display: flex; flex-direction: column; gap: 0.5rem; }
  .field { display: flex; flex-direction: column; gap: 0.15rem; font-size: 0.9rem; }
  .radio { display: inline-flex; align-items: center; gap: 0.3rem; font-weight: normal; }
  .hint { color: var(--text-muted); font-size: 0.8rem; }
  .modal-actions { display: flex; justify-content: flex-end; gap: 0.5rem; margin-top: 0.5rem; }

  .muted { color: var(--text-muted); font-style: italic; }

  .theme-options {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.9rem;
  }
  .theme-option {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    cursor: pointer;
  }

  .context-menu {
    position: fixed;
    list-style: none;
    margin: 0;
    padding: 0.25rem 0;
    background: var(--bg-surface);
    border: 1px solid var(--border-secondary);
    border-radius: 4px;
    box-shadow: var(--shadow);
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
  .menu-item:hover { background: var(--bg-accent-light); }
</style>
