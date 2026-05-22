# M5.3 — Tab bar + per-tab editor state

## Goal

**Before (post-M5.2):** `src/routes/+page.svelte` has a single right-pane
"surface": one `selectedDb`, one `sql` buffer, one `<Editor>`, one `active`
result. Selecting a different DB in the tree silently re-targets the same
buffer; running a query closes the prior result. The user has no way to keep
multiple queries side-by-side.

**After:** The right pane is a tab strip. Each tab is `{ id, serverId,
database, sql, dirty, lastError?, active? }`. Adding a tab (`+` button)
creates a tab pinned to the *currently selected* `(server, database)` with an
empty buffer. Closing a tab (`×`) drops it; closing the last tab leaves an
empty-state pane. Switching tabs reroutes Run / Cancel / Load-more / Close-result
to the focused tab's state. Each tab's title shows `server / database`, muted
when it matches the tree selection, highlighted when it doesn't (this is the
visual cue called out in `MILESTONES.md` for "the tab is targeting a different
DB than the tree is browsing").

Tree clicks no longer mutate any open tab's target. Instead, `selectedDb` is
purely a tree-selection cursor (used for the slot indicator, the right-click
context menu, and the *initial target* of newly-created tabs). Changing a
tab's database is an explicit action: right-click a tab → "Change database…"
opens a dialog letting the user pick from the connected server's databases.

This task is **frontend only** plus the introduction of a tiny new module
`src/lib/tabs.ts` for the data model and id generator. No backend changes.
`pnpm check` and the smoke test are the M5.3 acceptance signals.

## Current state

### `src/routes/+page.svelte` — the file this task rewrites the right-pane half of

Read it in full before starting. Key constructs that survive M5.3 unchanged:

- Left pane: connections tree, slot badges, password/add dialogs.
- `connectedState`, `tree`, the `Tree` component, `selectDb` *as a tree
  cursor*, `selectedConn`.
- The disconnect handler — extended slightly to also close every tab pinned
  to the disconnected server.
- `refreshSlotState`.

Key constructs that are **rewritten** in M5.3:

- `sql` (single `$state<string>`) → moves *into* each tab.
- `runningQuery` (single `$state<boolean>`) → moves *into* each tab.
- `active: ActiveResult | null` → moves *into* each tab.
- `lastError`, `loadingMore`, `editorWarning` → move *into* each tab.
- `runFromEditor`, `loadMore`, `closeActive`, `cancelRunning`,
  `statusLineText` → take a `Tab` parameter (or operate on the focused tab).

The current `selectDb`'s `await closeActive()` side effect is removed — tree
selection no longer touches results. Each tab's lifecycle is owned by the
tab itself.

### `src/lib/Editor.svelte` — already supports the multi-instance model

The `<Editor>` component is self-contained: each instance owns its own
`EditorView`, takes `initial`, emits `onChange(doc)`, fires `onRun(payload)`
on `Cmd+Enter`, and exposes `setDoc` / `focus` via `bind:this`. M5.3
instantiates one `<Editor>` per *visible* tab — but only the focused tab is
mounted at any time, to avoid wasting memory on N CodeMirror instances.
(Re-mounting on tab switch is cheap; CodeMirror's `state` is what's
expensive, and we keep `tab.sql` outside the editor as the source of truth.)

`getContext` is the bridge to the autocomplete completion source. Each tab
passes its own `(serverId, database)`; switching tabs swaps the context.

### `src/lib/ResultGrid.svelte` — unchanged

Each tab renders its own `<ResultGrid>` when it has an active result; the
component is stateless (props in, sort/widths state local). Multiple
unmounted tabs don't pay the grid's cost.

### `src/lib/schemaStore.ts` — unchanged

Schema payload cache is keyed by `(serverId, database)` — tabs sharing a
target share a cached payload automatically.

### Backend — entirely unchanged

`run_query`, `fetch_more`, `close_result`, `cancel_query`,
`disconnect_server` all stay as-is. The frontend's per-tab state is purely
client-side.

## Design choices baked into this spec

- **Tabs are in-memory only.** Per `MILESTONES.md`: "Tab IDs should be
  stable across reloads only if you bother to persist tabs — for M5,
  in-memory is fine; persistence can wait." If the user reloads, tabs are
  gone.
- **Tab id is a monotonically-increasing integer**, minted by a module-level
  counter in `tabs.ts`. No UUIDs needed; we never serialise tabs in M5.
- **Tree selection is a *cursor*, not a target.** It decides "where would a
  new tab open by default" and "which server's slot indicator is the right
  thing to highlight." It does **not** change any existing tab's target.
  This is the load-bearing visible-state principle the user will trip on
  if you get it wrong.
- **Each tab is pinned to `(serverId, database)`.** Changing the pin
  requires a right-click → "Change database…" action; there's no passive
  dropdown on the tab itself. Reason: a passive dropdown invites accidental
  re-targeting, which silently moves running queries to the wrong slot.
- **"Change database…" only offers databases on the *same server*.** Moving
  a tab to a different server is closing-and-reopening territory; the
  dialog deliberately doesn't expose it. (If a future task wants
  cross-server tab moves, that's its concern.)
- **The active tab's `<Editor>` is the only mounted CodeMirror instance.**
  Tab switch un-mounts the previous editor and mounts the next one. The
  per-tab `sql: string` is the persistence layer; `Editor.setDoc(tab.sql)`
  rehydrates on mount. Sort/widths state in `<ResultGrid>` is also
  per-mount — when the user switches tabs and back, the grid re-renders
  from `tab.active.rows` and sort/widths reset. Document this; it's a
  pragmatic trade-off, not a bug.
- **`dirty` is computed, not stored.** A tab is dirty if its current `sql`
  differs from its `initialSql` snapshot. Used in M5.4 by "Save as…"
  preselect logic; in M5.3 it just powers a `•` indicator in the tab title.
- **Closing a dirty tab shows no confirmation in M5.** Personal-use app;
  the user knows what they're doing. M6 polish can add one if it ever
  feels needed.
- **Cancel/Close-result on tab close.** If a tab with an `active` result
  is closed, fire `api.closeResult(rid)` before dropping the tab. If a tab
  with `runningQuery=true` is closed, fire `api.cancelQuery` first.
  Otherwise the backend leaks a slot until disconnect-sweep cleans it.
- **Slot indicator still reflects the *tree* selection, not the active
  tab.** Two cursors: the tree highlights the current browse target, and
  each tab carries its own pin. Showing the *tab's* slot status in the
  status line (we already do — `statusLineText`) covers the per-tab
  visibility; the tree badge stays as the per-server snapshot.
- **One tab at startup if `selectedDb` ever becomes non-null.** When the
  user first selects a DB from the tree (the previous "selectDb opens the
  pane" behaviour), open the first tab automatically. After that, tab
  creation is explicit via `+`.

## Deliverables

### 1. `src/lib/tabs.ts` — new module: tab data model + id generator

```ts
//! Tab model and id generator for the multi-tab right pane.
//!
//! Tabs are in-memory only; a reload clears them.  Each tab carries its own
//! editor buffer, runtime flags, and active result.  Tree selection is a
//! separate cursor (see `+page.svelte`'s `selectedDb`) — it does not mutate
//! any open tab.

import type { ColumnMeta, CommandError } from "./tauri";

export type ActiveResult = {
  resultId: string;
  columns: ColumnMeta[];
  rows: unknown[][];
  hasMore: boolean;
  rowCount: number;
  durationMs: number;
};

export type Tab = {
  /** Monotonic; assigned at creation. */
  id: number;

  /** Pin: tab targets this server's database, period.  Mutated only via
   *  the explicit "Change database…" action; never by tree selection. */
  serverId: number;
  database: string;

  /** Current editor buffer.  `<Editor>` is the source of truth while
   *  mounted; this is the value persisted across un-/re-mount. */
  sql: string;

  /** Snapshot of `sql` at tab creation (or last Save).  `dirty` is
   *  computed as `sql !== initialSql`. */
  initialSql: string;

  /** Set after a successful `run_query`; cleared by Close-result, Cancel,
   *  or DB change. */
  active: ActiveResult | null;

  /** Inline error from the last Run / Load more / Cancel. */
  lastError: CommandError | null;

  /** Warning above the inline error (multi-statement, empty buffer, etc.) */
  editorWarning: string | null;

  /** Set while a `run_query` is in flight. */
  runningQuery: boolean;

  /** Set while a `fetch_more` is in flight. */
  loadingMore: boolean;
};

let nextId = 1;

/** Create a new tab pinned to `(serverId, database)` with an empty buffer
 *  (or the supplied `sql`).  `initialSql` is set to the same value so the
 *  tab starts non-dirty. */
export function makeTab(
  serverId: number,
  database: string,
  sql: string = "",
): Tab {
  const id = nextId++;
  return {
    id,
    serverId,
    database,
    sql,
    initialSql: sql,
    active: null,
    lastError: null,
    editorWarning: null,
    runningQuery: false,
    loadingMore: false,
  };
}

/** Test-only: reset the id counter so test ids start at 1.  Not used by app code. */
export function __resetTabIds(): void {
  nextId = 1;
}
```

### 2. `src/lib/Tabs.svelte` — new component: tab strip

```svelte
<script lang="ts">
  //! Tab strip above the editor.  Pure presentation: events bubble up to
  //! the parent, which owns the tab list.

  import type { Tab } from "./tabs";

  let {
    tabs,
    activeId,
    treeServerId,
    treeDatabase,
    serverNameLookup,
    onSelect,
    onClose,
    onAdd,
    onChangeDatabase,
  }: {
    tabs: Tab[];
    activeId: number | null;
    /** The currently-selected server in the left tree (for "matches" styling). */
    treeServerId: number | null;
    treeDatabase: string | null;
    /** Resolve serverId → display name for the title. */
    serverNameLookup: (id: number) => string;
    onSelect: (id: number) => void;
    onClose: (id: number) => void;
    onAdd: () => void;
    onChangeDatabase: (id: number) => void;
  } = $props();

  function matchesTree(t: Tab): boolean {
    return t.serverId === treeServerId && t.database === treeDatabase;
  }

  function dirty(t: Tab): boolean {
    return t.sql !== t.initialSql;
  }

  function onTabContextMenu(e: MouseEvent, id: number) {
    e.preventDefault();
    // M5.3 ships only one menu action.  When more land (M5.4 may want
    // "Save as snippet"), promote to a real context menu component.
    onChangeDatabase(id);
  }

  function onMiddleClick(e: MouseEvent, id: number) {
    if (e.button === 1) {
      e.preventDefault();
      onClose(id);
    }
  }
</script>

<div class="tab-strip" role="tablist">
  {#each tabs as t (t.id)}
    {@const isActive = t.id === activeId}
    {@const muted = matchesTree(t)}
    <div
      class="tab"
      class:active={isActive}
      class:muted
      role="tab"
      aria-selected={isActive}
      onclick={() => onSelect(t.id)}
      onauxclick={(e) => onMiddleClick(e, t.id)}
      oncontextmenu={(e) => onTabContextMenu(e, t.id)}
      title="Right-click to change database"
    >
      <span class="server">{serverNameLookup(t.serverId)}</span>
      <span class="sep">/</span>
      <span class="db">{t.database}</span>
      {#if dirty(t)}<span class="dirty" aria-label="unsaved">•</span>{/if}
      <button
        class="close"
        aria-label="Close tab"
        onclick={(e) => { e.stopPropagation(); onClose(t.id); }}
      >×</button>
    </div>
  {/each}
  <button class="add" aria-label="New tab" onclick={onAdd}>+</button>
</div>

<style>
  .tab-strip {
    display: flex;
    gap: 0;
    border-bottom: 1px solid #ccc;
    background: #f7f7f7;
    align-items: stretch;
    overflow-x: auto;
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    padding: 0.35rem 0.6rem;
    border-right: 1px solid #ddd;
    cursor: pointer;
    font-size: 0.85rem;
    user-select: none;
    white-space: nowrap;
  }
  .tab:hover { background: #efefef; }
  .tab.active { background: white; border-bottom: 2px solid #3366cc; }
  .server { color: #333; font-weight: 500; }
  .sep { color: #aaa; }
  /* Muted = the tab matches the tree's current selection. */
  .tab.muted .server, .tab.muted .db { color: #888; }
  .tab:not(.muted) .db { color: #b14b00; font-weight: 600; }
  .dirty { color: #b14b00; padding-left: 0.15rem; }
  .close {
    margin-left: 0.4rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 1rem;
    line-height: 1;
    padding: 0 0.25rem;
    color: #888;
  }
  .close:hover { color: #b00020; background: #fde; border-radius: 2px; }
  .add {
    padding: 0.35rem 0.6rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 1rem;
    color: #666;
  }
  .add:hover { background: #efefef; color: #333; }
</style>
```

### 3. `src/routes/+page.svelte` — right-pane rewrite

Replace the single `sql / runningQuery / active / lastError / loadingMore /
editorWarning` state with a tab list and an `activeTabId`:

```ts
import Tabs from "$lib/Tabs.svelte";
import { makeTab, type Tab } from "$lib/tabs";

// Remove these (they move into Tab):
// let sql = $state("SELECT 1");
// let runningQuery = $state(false);
// let active = $state<ActiveResult | null>(null);
// let lastError = $state<CommandError | null>(null);
// let loadingMore = $state(false);
// let editorWarning = $state<string | null>(null);
// (Also remove the local `type ActiveResult` — re-exported from $lib/tabs.)

let tabs = $state<Tab[]>([]);
let activeTabId = $state<number | null>(null);

let activeTab = $derived(
  activeTabId === null ? null : tabs.find((t) => t.id === activeTabId) ?? null,
);

// Change-database dialog state
let dbDialog = $state<HTMLDialogElement | null>(null);
let dbDialogTabId = $state<number | null>(null);
let dbDialogPick = $state<string>("");
let dbDialogOptions = $state<string[]>([]);
let dbDialogError = $state<string>("");
```

**Tab lifecycle helpers** (add near the other handlers):

```ts
function addTab() {
  if (!selectedDb) return;
  const tab = makeTab(selectedDb.serverId, selectedDb.database, "");
  tabs.push(tab);
  activeTabId = tab.id;
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
```

**Rewrite `runFromEditor` / `loadMore` / `closeActive` / `cancelRunning` /
`statusLineText` to take the focused tab** (or use `activeTab`):

```ts
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
    const r: RunResult = await api.runQuery(tab.serverId, tab.database, payload.text);
    tab.active = {
      resultId: r.result_id,
      columns: r.columns,
      rows: r.first_chunk,
      hasMore: r.has_more,
      rowCount: r.row_count_so_far,
      durationMs: r.duration_ms_so_far,
    };
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

function statusLineText(tab: Tab): string {
  const a = tab.active!;
  const slot = connectedState[tab.serverId];
  const busy = slot ? slot.slots.filter((s) => s.busy).length : 0;
  const budget = slot ? slot.budget : 0;
  const serverName = connections.find((c) => c.id === tab.serverId)?.name ?? "?";
  const parts = [
    `${a.rowCount.toLocaleString()} rows`,
    `${a.durationMs}ms`,
    `slot [${busy}/${budget}]`,
    `${serverName}@${tab.database}`,
    a.hasMore ? "cursor open" : "cursor closed",
  ];
  return parts.join(" · ");
}
```

**Rewrite `selectDb`** — strip its side effects on result/active:

```ts
async function selectDb(serverId: number, database: string) {
  selectedDb = { serverId, database };
  // If there are no tabs yet, open the first one targeting this DB.
  if (tabs.length === 0) {
    addTab();
  }
}
```

**Extend the disconnect handler** to close every tab pinned to the
disconnected server:

```ts
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
  const node = tree.find((n) => n.conn.id === id);
  if (node) {
    node.children = null;
    node.expanded = false;
  }
  if (selectedDb?.serverId === id) selectedDb = null;
}
```

**Rewrite the right-pane markup**:

```svelte
<main class="right-pane">
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

  {#if activeTab}
    {@const tab = activeTab}
    {#key tab.id}
      <Editor
        bind:this={editor}
        initial={tab.sql}
        onChange={(doc) => { tab.sql = doc; }}
        onRun={(payload) => runFromEditor(payload)}
        getContext={() => ({ serverId: tab.serverId, database: tab.database })}
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
    </div>

    {#if tab.editorWarning}
      <p class="muted inline">{tab.editorWarning}</p>
    {/if}
    {#if tab.lastError}
      <p class="inline error">
        <span class="err-badge">{tab.lastError.kind}</span>
        {tab.lastError.message}
      </p>
    {/if}

    {#if tab.active}
      <ResultGrid
        columns={tab.active.columns}
        rows={tab.active.rows}
        statusLine={statusLineText(tab)}
        hasMore={tab.active.hasMore}
        loadingMore={tab.loadingMore}
        onLoadMore={loadMore}
        canLoadMore={!!tab.active.resultId}
      />
    {:else if !tab.runningQuery && !tab.lastError}
      <p class="muted">No active result. Press Run or Ctrl/Cmd+Enter.</p>
    {/if}

    {#if !isConnected(tab.serverId)}
      <p class="muted">Not connected. Right-click the server in the tree → Connect.</p>
    {/if}
  {:else}
    <p class="muted">Select a database in the tree (left), or click + to open a tab.</p>
  {/if}
</main>
```

**Helper for the button click path** (replaces the old `buildPayloadFromButton`):

```ts
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
```

**Change-database dialog** (place near the password / add-connection dialogs):

```svelte
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
```

## Implementation order

1. **`src/lib/tabs.ts`** — write first. Compiles standalone.
2. **`src/lib/Tabs.svelte`** — write next. Compiles standalone given
   `tabs.ts`. `pnpm check` should pass at this point (no consumers).
3. **`src/routes/+page.svelte`** — rewrite right-pane in this order:
   1. Add imports (`Tabs`, `makeTab`, `Tab`).
   2. Add the new state (`tabs`, `activeTabId`, change-db dialog refs)
      *next to* the old state. Don't delete the old state yet — the file
      won't compile mid-edit if you do.
   3. Add `activeTab` derived, `addTab`, `closeTab`, `selectTab`,
      `openChangeDbDialog`, `submitChangeDb`.
   4. Rewrite `runFromEditor`, `loadMore`, `closeActive`, `cancelRunning`,
      `statusLineText`, `buildPayloadFromButton`, `canRun` to operate on
      a `Tab`.
   5. Strip `selectDb`'s `closeActive` side effect.
   6. Extend `disconnect` to close server-pinned tabs.
   7. Swap the right-pane markup.
   8. Add the change-db dialog.
   9. Delete the now-unused state vars and the old `runFromEditor` shape.
4. `pnpm check` — clean.
5. Smoke test below.

## Known gotchas

- **`{#key tab.id}` around `<Editor>`** is the trick that gives each tab
  its own CodeMirror instance. Without `{#key ...}`, Svelte reuses the
  same `<Editor>` across tab switches, the `initial` prop becomes stale,
  and `setDoc` would have to be called manually on every switch. The
  `{#key}` block destroys and recreates the editor on tab change — cheap
  for a single editor, and it sidesteps the entire "swap the doc" code
  path. The `editor: Editor | undefined` bind is still useful for the
  current tab's imperative methods (focus on Cmd+T etc., M5.4).
- **CodeMirror state is lost on tab switch.** Undo history, scroll, fold
  state, selection — all reset when the user returns to a tab. The text
  is preserved via `tab.sql`. This is the trade-off vs keeping N
  CodeMirrors mounted; document it. Users who want persistent undo can
  stay on one tab.
- **ResultGrid sort/widths are lost on tab switch** for the same reason
  (the component unmounts). Re-sorting takes a click. Accept it for M5;
  M6 polish can hoist sort state into the tab if it becomes painful.
- **`tabs.push(tab)` works in Svelte 5 with `$state<Tab[]>([])`** because
  `$state` proxies arrays. Don't rewrite to `tabs = [...tabs, tab]` —
  that works too but adds churn. Either is fine; pick one and stick with
  it. The deletion path uses `tabs.filter(...)` because in-place splice
  is a subtle source of "I forgot to update the index" bugs.
- **`onauxclick` for middle-click close.** `auxclick` fires for any
  non-primary button; we check `e.button === 1` for middle. Some Linux
  setups bind middle-click to paste-from-selection on the page — that's
  the OS, not us; the close still fires.
- **Right-click on tab does NOT use a generic context menu.** M5.3 has
  exactly one action ("Change database…"); a full context menu would be
  ceremony. When M5.4 wants "Save query…" on a tab, promote to a real
  context menu component.
- **Change-DB dialog uses `api.listDatabases`** — which acquires a slot
  briefly. Per AGENTS.md principle 1, this is fine: it's a direct user
  action (the user opened the dialog). Slot indicator may briefly bump
  to `[1/2]`.
- **Deletion of a tab targeting a server that's *already* disconnected**
  must not call `api.cancelQuery` or `api.closeResult` (both would fail
  with `NotConnected`). The current code path checks `tab.runningQuery`
  and `tab.active?.resultId` but not connection state — the API calls
  are wrapped in `try/catch` so failures are swallowed. Fine.
- **`activeTab` is `$derived`, not `$state`.** Don't mutate it; mutate
  the underlying `tabs[i]`. Svelte's reactivity tracks reads on the proxy
  fields, so `tab.sql = doc` from inside `<Editor>`'s `onChange` updates
  the right tab in-place.
- **Empty-state.** With `tabs.length === 0` and `selectedDb === null`,
  the empty-state message is shown. With `selectedDb !== null` and zero
  tabs, the first `selectDb` call auto-creates a tab. After that, tab
  creation is `+` only.
- **`canRun` no longer derives from `$derived`** — it's a function that
  takes a tab. Reason: each tab's `canRun` depends on its own fields;
  collapsing them into a single derived value pollutes one tab's state
  with another's runningQuery flag. Inline the function call in the
  markup.
- **Don't close result when the tree selection changes.** The old
  `selectDb` did `await closeActive()`; remove it. Tree selection is now
  decoupled from any tab's lifecycle.
- **Hover hint on tabs.** The `title="Right-click to change database"` is
  load-bearing UX — without it the only discoverable way to re-target a
  tab is `MILESTONES.md`. Keep it short; M6 polish may swap to a real
  tooltip with the full server / database path.
- **Tab strip overflow.** When the user opens many tabs, the strip
  scrolls horizontally (`overflow-x: auto`). v1 doesn't add a tab
  dropdown / chevron overflow indicator; that's M6.
- **Slot indicator on the tab strip:** intentionally absent. Each tab's
  status line already shows `slot [busy/budget]`. Putting per-tab
  slot badges on the strip would be visual noise. The tree's per-server
  badge stays as the canonical indicator.

## Tests

`pnpm check` is the gate. No new automated tests in M5.3 (no Vitest
configured).

### Manual smoke test

```bash
docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17
./run.sh
```

1. Connect to the local Postgres. Click the `postgres` DB. **One tab
   opens automatically** pinned to `postgres`.
2. Run `SELECT 1`. Grid shows 1 row. Tab title is muted (matches tree).
3. Click `+`. **Second tab opens**, also pinned to `postgres` (same as
   tree). Both tabs muted.
4. Click the `template1` DB in the tree. **Both tabs stay pinned to
   `postgres`** — they are now highlighted (don't match tree). Status
   line in the active tab still says `postgres`.
5. In the active tab, run `SELECT 2`. Grid shows 2. Switch to tab #1
   (`SELECT 1`). Its grid is no longer mounted — empty-state shows.
   Switch back: tab #2's result is gone too.

   **This is the documented trade-off.** Sort/scroll state and the grid
   itself unmount on tab switch.
6. Type a long query into tab #2. Switch to tab #1, back. Tab #2's text
   is preserved (via `tab.sql`); CodeMirror undo history resets.
7. Right-click tab #1 → "Change database…". Dialog opens with the list
   of databases on this server. Pick `template1`. Tab #1 title now reads
   `local / template1`, muted (matches tree). The result (if any) was
   closed; the buffer is preserved.
8. Run `SELECT current_database()` in tab #1. Returns `template1`.
   Switch to tab #2; run the same query. Returns `postgres`. Confirms
   tabs are independently pinned.
9. Middle-click a tab. **Tab closes.** Focus moves to a sibling.
10. Click the `×` on the last open tab. Tab closes. Empty-state shows:
    "Select a database in the tree (left), or click + to open a tab."
11. Right-click the server → Disconnect. Any remaining tabs targeting
    that server close; the tree node shows disconnected; new tabs can't
    be opened against it until reconnect. The slot badge returns to
    `[0/2]`.
12. Open many tabs (>10). The tab strip scrolls horizontally; closing
    tabs reflows.
13. Cmd+Enter inside the editor still runs against the *active tab's*
    pin, even if you've clicked the tree elsewhere.

## Acceptance criteria

- [ ] `pnpm check` succeeds clean.
- [ ] `git status` shows two new files: `src/lib/tabs.ts`, `src/lib/Tabs.svelte`.
- [ ] `grep -F "let sql = \$state" src/routes/+page.svelte` returns zero
      matches — the single-buffer state must be gone.
- [ ] `grep -F "let active = \$state" src/routes/+page.svelte` returns zero
      matches.
- [ ] `grep -F "let tabs = \$state" src/routes/+page.svelte` returns one
      match.
- [ ] `grep -F "{#key tab.id}" src/routes/+page.svelte` matches — the
      Editor remount-per-tab pattern is in place.
- [ ] `grep -F "selectedDb" src/routes/+page.svelte` shows uses limited to
      *tree selection*, *new-tab default*, and the **disconnect / context
      menu** — never to mutate an existing tab.
- [ ] `grep -F "closeActive" src/routes/+page.svelte` shows no call inside
      `selectDb` — tree clicks no longer touch any tab's active result.
- [ ] Smoke step 4 — tab pinning verified (tree click does not re-target).
- [ ] Smoke step 7 — Change-database dialog works.
- [ ] Smoke step 8 — independent pins confirmed by `SELECT current_database()`.
- [ ] Smoke step 9 — middle-click closes.
- [ ] Smoke step 11 — disconnect closes server-pinned tabs.
- [ ] No backend changes; `git diff src-tauri/` is empty.
- [ ] No new pnpm or cargo dependencies.

## Out of scope

- Persisting tabs across reloads — explicitly deferred ("in-memory is fine
  for M5" per `MILESTONES.md`).
- A real context menu component on tabs (currently right-click maps
  directly to Change-database) — promote in M5.4 if the Saved panel wants
  "Save query as…" on a tab.
- Drag-to-reorder tabs — v1.1.
- Cross-server tab moves — explicitly excluded ("Change database…" is
  same-server only).
- Slot indicator on each tab — intentionally absent; status line covers it.
- Preserving sort / scroll / undo across tab switches — accepted trade-off
  vs keeping N CodeMirrors mounted.
- Close-tab confirmation when dirty — M6 polish if it ever matters.
- History / Saved double-click → new tab — **M5.4** consumes this task's
  `makeTab` + `tabs.push` to wire it up.
- CSV export on the active tab's grid — **M5.5**.
