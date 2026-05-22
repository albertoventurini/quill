# M5.4 — History + Saved side panel

## Goal

**Before (post-M5.3):** The backend tracks history (`query_history`) and
saved queries (`saved_queries`) and exposes `list_history`, `clear_history`,
`list_saved`, `save_query`, `delete_saved`, `rename_saved`. The frontend has
a working multi-tab right pane but no UI to surface either table — every
executed query lands in history invisibly, and there's no way to save the
active tab's buffer as a named snippet.

**After:** The left pane gains a side panel below the connection tree with
two sub-tabs: **History** and **Saved**. History lists every executed query,
newest first, filterable to the current server; failed rows show in red.
Double-click on a history entry opens a new editor tab pinned to the
*original* `(server, database)` with the SQL pre-filled. Saved lists global
and server-scoped snippets; double-click opens a new tab the same way. The
header strip on the editor tab gains a **Save…** button that opens a
"Save query" dialog seeded with the active tab's buffer; the dialog chooses
global vs server scope and writes via `save_query`.

The side panel polls only on explicit user action — when the panel is
revealed, when the user clicks Refresh, or when local writes (Save, Delete,
Rename, Clear history) succeed. There is **no background refresh** —
principle 1 still holds: history rows that land while the panel is open
appear on the next refresh, not via a push event.

This task is **frontend only**. `pnpm check` and the smoke test are the
M5.4 acceptance signals.

## Current state

### `src-tauri/src/commands/mod.rs` — M5.2's six commands are live

```
list_history(limit?, server_id?)   -> HistoryRecord[]
clear_history()                    -> ()
list_saved(server_id?)             -> SavedQuery[]
save_query(NewSavedQuery)          -> SavedQuery
delete_saved(id)                   -> ()
rename_saved(id, newName)          -> SavedQuery
```

TS bindings in `src/lib/tauri.ts` already have `api.listHistory`,
`api.clearHistory`, `api.listSaved`, `api.saveQuery`, `api.deleteSaved`,
`api.renameSaved` plus the `HistoryRecord`, `SavedQuery`, `NewSavedQuery`,
`SavedScope` types. M5.4 consumes them; it does not add any.

### `src/routes/+page.svelte` — the file this task edits

Post-M5.3 the left pane is one big block:

```
┌────── left-pane ───┐
│ Connections header │
│ [+ Add]            │
│ Tree (servers/db/  │
│ schemas/relations) │
└────────────────────┘
```

M5.4 splits it vertically:

```
┌────── left-pane ───┐
│ Connections header │
│ Tree               │
├────────────────────┤
│ [History|Saved]    │
│ list of records    │
└────────────────────┘
```

### `src/lib/tabs.ts` — the new-tab helper M5.4 calls

`makeTab(serverId, database, sql)` from M5.3 is exactly the entry point a
double-click handler needs. The Saved/History double-click flow becomes:

```ts
const tab = makeTab(record.server_id, record.database, record.sql);
tabs.push(tab);
activeTabId = tab.id;
```

This is the only contract M5.4 needs from the tab layer.

### What if the target server isn't connected?

A history entry references `(server_id, database)` that may belong to a
server the user has since disconnected. Opening a tab against it is still
useful: the buffer is pre-filled and the user can press Connect via the
tree's right-click. The tab simply shows the existing "Not connected"
hint until they do. **Don't auto-prompt for password on double-click**;
that would be a surprising side effect.

### What if the target server has been deleted?

If `record.server_id` no longer exists in `connections`, opening a tab is
meaningless — there's no way to connect, and the tree won't show the
server. M5.4 detects this on double-click and shows an inline error
("This connection has been deleted") inside the side panel rather than
opening a broken tab.

## Design choices baked into this spec

- **Two sub-tabs, not two panels.** Saving vertical space matters; a
  combined panel with a tab switch is the right shape. The tree owns the
  upper half (variable height); the side panel owns the lower half with
  a fixed-but-resizable bias (set `min-height: 240px` in CSS and let it
  grow).
- **No background refresh.** Reloading the lists is a user action:
  opening the panel, switching sub-tabs, clicking Refresh, or completing
  a local write. Per `MILESTONES.md` §M5: "no background work."
- **History limit defaults to 200 in the UI**, not the backend's
  `HISTORY_RETENTION` (1000). The full 1000 rows are available via Load
  more inside the panel (one click). Reason: rendering 1000 history rows
  on every open is noisy and slow.
- **Server-filter chip on the history list.** Three states: All / Current
  server / (collapsed when no `selectedDb`). Clicking the chip cycles. No
  multi-server selection.
- **Saved list scope filter is implicit.** When `selectedDb` is set, the
  Saved sub-tab shows global rows + that server's rows (handled by
  `list_saved(serverId)`). When no server is selected, the Saved sub-tab
  shows global rows only (`list_saved(null)`). This matches `saved::list`'s
  semantics from M5.1.
- **Failed history rows are visually distinguished, not separated.** Red
  text + a small `✕` glyph in front. Don't filter them out — failed runs
  are exactly the queries the user wants to re-edit.
- **Double-click is the primary open action.** Single-click selects (used
  by keyboard navigation in M6); Enter on a selected row also opens.
  Don't double-up double-click with "open in current tab" — *new tab
  always* for both. Reason: opening in the active tab would silently
  overwrite the user's buffer; we don't have an undo path for that.
- **Save dialog is modal**, like Add Connection. Two text inputs (name,
  optional notes — *no, just name in M5*) and a scope radio. Closes on
  Save / Cancel.
- **Rename is inline.** Click a pencil icon next to the saved row; the
  name becomes an input; Enter commits, Escape cancels. No modal — too
  much ceremony for a frequent action.
- **Delete is a button next to rename.** No confirmation in M5. Personal
  app; restorable via the cleared history? No — once deleted, gone. M6
  polish can add an undo.
- **Clear history is a confirmation-required action.** Single button at
  the bottom of the History list; click → `<dialog>` with "Are you sure?
  This deletes all N rows." Hard to undo; one click of friction is
  appropriate.
- **Truncate SQL preview in the list to one line, ~80 chars.** Full SQL
  rendered on hover via `title=""` attribute; double-click to open
  populates the full SQL in the new tab. Don't try to render multi-line
  SQL inline.
- **`ts` rendering is "Today 14:32" / "Yesterday 09:15" / "Mar 12 14:32"
  / "2024-12-01 14:32"** depending on age. Use a small local helper, not
  a date library. The grid's tabular-nums style helps alignment.

## Deliverables

### 1. `src/lib/SidePanel.svelte` — new component: side panel with sub-tabs

```svelte
<script lang="ts">
  //! Side panel with two sub-tabs: History (executed queries) and Saved
  //! (named snippets).  Loads on mount and on explicit refresh; never
  //! polls.

  import { api, type HistoryRecord, type SavedQuery, type SavedScope } from "./tauri";
  import { errorMessage } from "./tree";

  let {
    selectedServerId,
    onOpenInNewTab,
    onError,
  }: {
    selectedServerId: number | null;
    onOpenInNewTab: (serverId: number, database: string, sql: string) => void;
    onError: (msg: string) => void;
  } = $props();

  type SubTab = "history" | "saved";
  let subtab = $state<SubTab>("history");

  // ── History state ──
  let history = $state<HistoryRecord[]>([]);
  let historyFilter = $state<"all" | "current">("all");
  let historyLoading = $state(false);

  // ── Saved state ──
  let saved = $state<SavedQuery[]>([]);
  let savedLoading = $state(false);
  let renameId = $state<number | null>(null);
  let renameDraft = $state("");

  // ── Clear-history confirmation ──
  let clearDialog = $state<HTMLDialogElement | null>(null);

  $effect(() => {
    refresh();
  });

  // Re-fetch when the selected server changes — the filter is server-aware.
  $effect(() => {
    void selectedServerId;
    if (subtab === "saved") void refreshSaved();
    if (subtab === "history" && historyFilter === "current") void refreshHistory();
  });

  // Re-fetch on subtab switch.
  $effect(() => {
    void subtab;
    if (subtab === "history") void refreshHistory();
    else void refreshSaved();
  });

  async function refresh() {
    if (subtab === "history") await refreshHistory();
    else await refreshSaved();
  }

  async function refreshHistory() {
    historyLoading = true;
    try {
      const serverId = historyFilter === "current" ? selectedServerId : null;
      history = await api.listHistory(200, serverId);
    } catch (err) {
      onError(errorMessage(err));
    } finally {
      historyLoading = false;
    }
  }

  async function refreshSaved() {
    savedLoading = true;
    try {
      saved = await api.listSaved(selectedServerId);
    } catch (err) {
      onError(errorMessage(err));
    } finally {
      savedLoading = false;
    }
  }

  function cycleHistoryFilter() {
    historyFilter = historyFilter === "all" ? "current" : "all";
    void refreshHistory();
  }

  // ── Open in new tab ──

  async function openHistory(record: HistoryRecord) {
    onOpenInNewTab(record.server_id, record.database, record.sql);
  }

  async function openSaved(record: SavedQuery) {
    // Saved scope=global has no server pin; default to the tree's selection.
    const sid = record.server_id ?? selectedServerId;
    if (sid === null) {
      onError("Select a server in the tree before opening a global snippet.");
      return;
    }
    // For global snippets, we don't know which database to target.  Use a
    // sensible default: the connection's default_db.  We don't have it here
    // without an extra fetch, so use 'postgres' as a fallback for v1; the
    // user can right-click → Change database on the new tab.
    const database = record.scope === "global" ? "postgres" : "<unknown>"; // see note below
    onOpenInNewTab(sid, database, record.sql);
  }

  // ── Rename / Delete saved ──

  function beginRename(row: SavedQuery) {
    renameId = row.id;
    renameDraft = row.name;
  }

  async function commitRename(row: SavedQuery, e?: Event) {
    e?.preventDefault();
    const next = renameDraft.trim();
    renameId = null;
    if (!next || next === row.name) return;
    try {
      await api.renameSaved(row.id, next);
      await refreshSaved();
    } catch (err) {
      onError(errorMessage(err));
    }
  }

  function cancelRename() {
    renameId = null;
    renameDraft = "";
  }

  async function deleteSaved(row: SavedQuery) {
    try {
      await api.deleteSaved(row.id);
      await refreshSaved();
    } catch (err) {
      onError(errorMessage(err));
    }
  }

  // ── Clear history ──

  function askClear() {
    clearDialog?.showModal();
  }

  async function confirmClear() {
    try {
      await api.clearHistory();
      clearDialog?.close();
      await refreshHistory();
    } catch (err) {
      clearDialog?.close();
      onError(errorMessage(err));
    }
  }

  // ── Formatting helpers ──

  function shortSql(sql: string): string {
    const oneLine = sql.replace(/\s+/g, " ").trim();
    return oneLine.length > 80 ? oneLine.slice(0, 77) + "…" : oneLine;
  }

  /** "Today 14:32" / "Yesterday 09:15" / "Mar 12 14:32" / "2024-12-01 14:32" */
  function formatTs(ts: string): string {
    // SQLite's datetime('now') is UTC, no timezone marker.  Append Z to parse.
    const d = new Date(ts.endsWith("Z") ? ts : ts + "Z");
    const now = new Date();
    const same = (a: Date, b: Date) =>
      a.getFullYear() === b.getFullYear() &&
      a.getMonth() === b.getMonth() &&
      a.getDate() === b.getDate();
    const yest = new Date(now.getTime() - 86_400_000);
    const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
    if (same(d, now)) return `Today ${hm}`;
    if (same(d, yest)) return `Yesterday ${hm}`;
    if (d.getFullYear() === now.getFullYear()) {
      const mon = d.toLocaleString(undefined, { month: "short" });
      return `${mon} ${d.getDate()} ${hm}`;
    }
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${hm}`;
  }
  function pad(n: number): string {
    return n < 10 ? `0${n}` : `${n}`;
  }
</script>

<div class="panel">
  <div class="sub-tabs">
    <button
      class="sub-tab"
      class:active={subtab === "history"}
      onclick={() => (subtab = "history")}
    >History</button>
    <button
      class="sub-tab"
      class:active={subtab === "saved"}
      onclick={() => (subtab = "saved")}
    >Saved</button>
    <button class="refresh" onclick={refresh} title="Refresh">⟳</button>
  </div>

  {#if subtab === "history"}
    <div class="toolbar">
      <button class="chip" onclick={cycleHistoryFilter} title="Toggle filter">
        {historyFilter === "all" ? "All servers" : "Current server only"}
      </button>
      <button class="chip danger" onclick={askClear} disabled={history.length === 0}>
        Clear history
      </button>
    </div>
    <div class="list">
      {#if historyLoading}
        <p class="muted">Loading…</p>
      {:else if history.length === 0}
        <p class="muted">No history yet.</p>
      {:else}
        {#each history as r (r.id)}
          <button
            class="row history"
            class:failed={!r.ok}
            ondblclick={() => openHistory(r)}
            title={r.sql}
          >
            <span class="ts">{formatTs(r.ts)}</span>
            <span class="db">{r.database}</span>
            <span class="sql">{r.ok ? "" : "✕ "}{shortSql(r.sql)}</span>
            <span class="duration">{r.duration_ms}ms</span>
          </button>
        {/each}
      {/if}
    </div>
  {:else}
    <div class="list">
      {#if savedLoading}
        <p class="muted">Loading…</p>
      {:else if saved.length === 0}
        <p class="muted">No saved snippets yet. Click <strong>Save…</strong> on a tab to add one.</p>
      {:else}
        {#each saved as r (r.id)}
          <div class="row saved">
            {#if renameId === r.id}
              <form class="rename-form" onsubmit={(e) => commitRename(r, e)}>
                <!-- svelte-ignore a11y_autofocus -->
                <input class="rename-input" bind:value={renameDraft} autofocus onkeydown={(e) => { if (e.key === "Escape") cancelRename(); }} />
              </form>
            {:else}
              <button class="row-button" ondblclick={() => openSaved(r)} title={r.sql}>
                <span class="scope">{r.scope === "global" ? "🌐" : "🔒"}</span>
                <span class="name">{r.name}</span>
                <span class="sql">{shortSql(r.sql)}</span>
              </button>
            {/if}
            {#if renameId !== r.id}
              <button class="icon-button" title="Rename" onclick={() => beginRename(r)}>✎</button>
              <button class="icon-button" title="Delete" onclick={() => deleteSaved(r)}>🗑</button>
            {/if}
          </div>
        {/each}
      {/if}
    </div>
  {/if}
</div>

<dialog bind:this={clearDialog} class="modal">
  <h2>Clear history?</h2>
  <p>This permanently deletes every history row. Saved snippets are not affected.</p>
  <div class="modal-actions">
    <button class="btn" onclick={() => clearDialog?.close()}>Cancel</button>
    <button class="btn btn-danger" onclick={confirmClear}>Clear</button>
  </div>
</dialog>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    border-top: 1px solid #ccc;
    min-height: 240px;
    overflow: hidden;
  }
  .sub-tabs {
    display: flex;
    gap: 0;
    align-items: stretch;
    border-bottom: 1px solid #ddd;
    background: #f0f0f0;
  }
  .sub-tab {
    padding: 0.35rem 0.6rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font: inherit;
    font-size: 0.85rem;
    color: #555;
  }
  .sub-tab.active { background: white; border-bottom: 2px solid #3366cc; color: #111; }
  .refresh {
    margin-left: auto;
    padding: 0.25rem 0.5rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 0.95rem;
    color: #555;
  }
  .refresh:hover { color: #111; }

  .toolbar {
    display: flex;
    gap: 0.4rem;
    padding: 0.35rem 0.5rem;
    border-bottom: 1px solid #eee;
    background: #fafafa;
  }
  .chip {
    padding: 0.15rem 0.5rem;
    border: 1px solid #bbb;
    border-radius: 99px;
    background: white;
    cursor: pointer;
    font-size: 0.75rem;
  }
  .chip:hover { background: #f0f0f0; }
  .chip.danger { border-color: #d88; color: #b00020; }
  .chip:disabled { opacity: 0.5; cursor: not-allowed; }

  .list {
    overflow-y: auto;
    flex: 1;
    padding: 0.25rem 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    padding: 0.25rem 0.6rem;
    background: transparent;
    border: none;
    text-align: left;
    cursor: pointer;
    font: inherit;
    font-size: 0.8rem;
    font-variant-numeric: tabular-nums;
  }
  .row:hover { background: #f3f3f7; }
  .row.failed { color: #b00020; }
  .row.failed .sql { color: #b00020; }
  .ts { color: #666; min-width: 7em; }
  .db { color: #06536b; min-width: 6em; }
  .sql {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  }
  .duration { color: #888; min-width: 4em; text-align: right; }

  .row.saved { padding: 0.15rem 0.6rem; }
  .row-button {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.45rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font: inherit;
    font-size: 0.85rem;
    text-align: left;
    padding: 0.1rem 0;
    overflow: hidden;
  }
  .scope { font-size: 0.85rem; }
  .name { font-weight: 600; min-width: 8em; }

  .icon-button {
    background: transparent;
    border: none;
    cursor: pointer;
    color: #777;
    padding: 0.15rem 0.3rem;
    font-size: 0.85rem;
  }
  .icon-button:hover { color: #111; background: #eee; border-radius: 3px; }

  .rename-form { flex: 1; }
  .rename-input {
    width: 100%;
    padding: 0.2rem 0.35rem;
    border: 1px solid #888;
    border-radius: 3px;
    font: inherit;
  }

  .muted { color: #888; font-style: italic; padding: 0.5rem 0.6rem; font-size: 0.85rem; }

  /* Local modal style; the global ones live in +page.svelte */
  .modal { border: 1px solid #888; border-radius: 8px; padding: 1.25rem; max-width: 360px; }
  .modal::backdrop { background: rgba(0,0,0,0.3); }
  .modal-actions { display: flex; justify-content: flex-end; gap: 0.5rem; margin-top: 0.75rem; }
  .btn { padding: 0.3rem 0.6rem; border: 1px solid #888; border-radius: 4px; background: #f0f0f0; cursor: pointer; font: inherit; font-size: 0.9rem; }
  .btn:hover { background: #e0e0e0; }
  .btn-danger { background: #b00020; color: white; border-color: #8a0019; }
  .btn-danger:hover { background: #8a0019; }
</style>
```

### 2. `src/lib/SaveDialog.svelte` — new component: "Save query" modal

A small dialog that takes the active tab's buffer and writes a new
`saved_queries` row. Self-contained so `+page.svelte` stays readable.

```svelte
<script lang="ts">
  import { api, type CommandError, type NewSavedQuery, type SavedScope } from "./tauri";
  import { errorMessage } from "./tree";

  let {
    initialSql,
    serverIdHint,
    onSaved,
    onError,
  }: {
    initialSql: string;
    /** The tab's serverId — used for the "Server only" scope option. `null`
     *  means there's no active tab; the scope defaults to global. */
    serverIdHint: number | null;
    onSaved: () => void;
    onError: (msg: string) => void;
  } = $props();

  let dialog = $state<HTMLDialogElement | null>(null);
  let name = $state("");
  let scope = $state<SavedScope>(serverIdHint === null ? "global" : "server");
  let formError = $state("");

  export function open(): void {
    name = "";
    formError = "";
    scope = serverIdHint === null ? "global" : "server";
    dialog?.showModal();
  }

  async function submit(e: Event) {
    e.preventDefault();
    formError = "";
    if (!name.trim()) {
      formError = "Name is required.";
      return;
    }
    const payload: NewSavedQuery = {
      name: name.trim(),
      scope,
      server_id: scope === "server" ? serverIdHint : null,
      sql: initialSql,
    };
    if (scope === "server" && payload.server_id === null) {
      formError = "No active server — pick a tab first, or choose Global.";
      return;
    }
    try {
      await api.saveQuery(payload);
      dialog?.close();
      onSaved();
    } catch (err) {
      const ce = err as CommandError;
      // Friendlier duplicate-name path.
      if (ce.kind === "Saved" && ce.message.includes("already exists")) {
        formError = ce.message;
      } else {
        onError(errorMessage(err));
        dialog?.close();
      }
    }
  }
</script>

<dialog bind:this={dialog} class="modal">
  <h2>Save query</h2>
  <form onsubmit={submit} class="save-form">
    <label class="field">
      Name
      <!-- svelte-ignore a11y_autofocus -->
      <input class="input" bind:value={name} autofocus required />
    </label>
    <fieldset class="scope">
      <legend>Scope</legend>
      <label>
        <input type="radio" name="scope" value="global" bind:group={scope} />
        Global (visible to every server)
      </label>
      <label>
        <input
          type="radio"
          name="scope"
          value="server"
          bind:group={scope}
          disabled={serverIdHint === null}
        />
        This server only
      </label>
    </fieldset>
    {#if formError}<p class="error">{formError}</p>{/if}
    <div class="modal-actions">
      <button type="button" class="btn" onclick={() => dialog?.close()}>Cancel</button>
      <button type="submit" class="btn btn-primary">Save</button>
    </div>
  </form>
</dialog>

<style>
  .modal { border: 1px solid #888; border-radius: 8px; padding: 1.25rem; max-width: 400px; width: 90%; }
  .modal::backdrop { background: rgba(0,0,0,0.3); }
  .save-form { display: flex; flex-direction: column; gap: 0.75rem; }
  .field { display: flex; flex-direction: column; gap: 0.15rem; font-size: 0.9rem; }
  .input { padding: 0.35rem; border: 1px solid #aaa; border-radius: 4px; font: inherit; }
  .scope { display: flex; flex-direction: column; gap: 0.25rem; font-size: 0.85rem; border: 1px solid #ddd; padding: 0.5rem; border-radius: 4px; }
  .scope legend { padding: 0 0.25rem; color: #555; }
  .modal-actions { display: flex; justify-content: flex-end; gap: 0.5rem; margin-top: 0.5rem; }
  .btn { padding: 0.3rem 0.6rem; border: 1px solid #888; border-radius: 4px; background: #f0f0f0; cursor: pointer; font: inherit; font-size: 0.9rem; }
  .btn:hover { background: #e0e0e0; }
  .btn-primary { background: #3366cc; color: white; border-color: #2255aa; }
  .btn-primary:hover { background: #2255aa; }
  .error { color: #b00020; font-size: 0.85rem; margin: 0; }
</style>
```

### 3. `src/routes/+page.svelte` — wire SidePanel + SaveDialog

**Imports:**

```ts
import SidePanel from "$lib/SidePanel.svelte";
import SaveDialog from "$lib/SaveDialog.svelte";
```

**New state for the side-panel error toast and save dialog ref:**

```ts
let sidePanelError = $state<string | null>(null);
let saveDialog = $state<SaveDialog | undefined>(undefined);
let savedListRefreshKey = $state(0); // bump after Save to force SidePanel re-fetch
```

**Side-panel "open in new tab" handler:**

```ts
function openInNewTab(serverId: number, database: string, sql: string) {
  // Guard: if the connection has been deleted, surface it.
  if (!connections.some((c) => c.id === serverId)) {
    sidePanelError = "This connection has been deleted.";
    return;
  }
  const tab = makeTab(serverId, database, sql);
  tabs.push(tab);
  activeTabId = tab.id;
}
```

**Save button handler:**

```ts
function openSaveDialog() {
  if (!activeTab) return;
  saveDialog?.open();
}
```

**Append the side panel to the left pane:**

```svelte
<aside class="left-pane">
  <!-- existing connection tree block stays here -->

  <SidePanel
    selectedServerId={selectedDb?.serverId ?? null}
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
```

**Add Save… button to the action row:**

```svelte
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
```

**Mount the SaveDialog once below the active tab markup (or near the
other dialogs):**

```svelte
{#if activeTab}
  {@const tab = activeTab}
  <SaveDialog
    bind:this={saveDialog}
    initialSql={tab.sql}
    serverIdHint={tab.serverId}
    onSaved={() => { savedListRefreshKey += 1; }}
    onError={(msg) => (sidePanelError = msg)}
  />
{/if}
```

To force the SidePanel to refresh after Save, pass `savedListRefreshKey`
as a prop and `$effect` on it inside the panel — or simpler: keep the
ref reactive via Svelte's recursion (the side panel already refreshes on
subtab/server change; an explicit Refresh button covers the case
otherwise). Pragmatic v1 choice: after Save, the user clicks the panel's
`⟳` button or switches sub-tabs. If you want auto-refresh, add a
`refreshTrigger` prop and a `$effect(() => { void refreshTrigger; refresh(); })`
inside `SidePanel`.

For M5.4 ship with **explicit refresh** (the user clicks `⟳` after Save
to see their new snippet) and document the trade-off. The dialog could
trivially be enhanced later.

### 4. Saved-snippet "database" defaulting (open question, resolve in this task)

Global saved snippets are scope-only — they don't pin a database. When
the user double-clicks a global snippet, M5.4 needs to decide which
database to target:

- **Option A (current draft above):** default to `"postgres"`. Simple.
  Wrong half the time.
- **Option B (recommended):** default to `selectedDb?.database` if a tree
  selection is set, otherwise the saved connection's `default_db`. This
  requires fetching `connections` in the SidePanel (cheap) or passing
  `defaultDbFor(serverId)` from the parent.

Pick **Option B**. The handler becomes:

```ts
function defaultDbForServer(serverId: number): string {
  return connections.find((c) => c.id === serverId)?.default_db ?? "postgres";
}

function openInNewTab(serverId: number, database: string, sql: string) {
  if (!connections.some((c) => c.id === serverId)) {
    sidePanelError = "This connection has been deleted.";
    return;
  }
  // For global snippets, the SidePanel calls with database="" and we resolve here.
  const targetDb = database || (selectedDb?.serverId === serverId ? selectedDb.database : defaultDbForServer(serverId));
  const tab = makeTab(serverId, targetDb, sql);
  tabs.push(tab);
  activeTabId = tab.id;
}
```

And update `SidePanel.svelte`'s `openSaved` to pass `""` for the
database when the saved row is `global`:

```ts
async function openSaved(record: SavedQuery) {
  const sid = record.server_id ?? selectedServerId;
  if (sid === null) {
    onError("Select a server in the tree before opening a global snippet.");
    return;
  }
  const db = record.scope === "global" ? "" : record.database ?? "";
  // `record.database` doesn't actually exist on SavedQuery — global has no
  // DB at all.  Pass "" and let the parent resolve.
  onOpenInNewTab(sid, db, record.sql);
}
```

(Saved snippets don't store a database — only server-scope snippets are
tied to a server, but they're still db-agnostic. The "resolve to a
sensible default DB" logic lives in `+page.svelte`.)

## Implementation order

1. **`src/lib/SidePanel.svelte`** — write first. `pnpm check` may complain
   until it's consumed in step 4 (unused `import` warnings), or it may not
   (depends on svelte-check config). Either way, the file compiles.
2. **`src/lib/SaveDialog.svelte`** — write next.
3. **`src/routes/+page.svelte`**:
   1. Add imports + new state vars.
   2. Add `openInNewTab` + `defaultDbForServer` + `openSaveDialog`.
   3. Append `<SidePanel>` to the left pane.
   4. Add the Save… button to the action row.
   5. Mount `<SaveDialog>` near the other dialogs.
4. `pnpm check` — clean.
5. Smoke test below.

## Known gotchas

- **SQLite timestamps are UTC without a TZ marker.** `datetime('now')`
  produces `"2026-05-22 14:32:00"` (no `Z`). `new Date(ts)` parses it as
  *local* time on some browsers — wrong. The fix is the `ts.endsWith("Z") ? ts : ts + "Z"` shim in
  `formatTs`. Verify by running a query at 23:55 local; the timestamp
  should not flip to "Yesterday".
- **`bind:group={scope}` on radios.** Svelte 5 still supports it. The
  `name="scope"` attribute is required for radio behaviour; don't omit.
- **`$effect(() => { void subtab; ... })`** — the `void` consumes the
  read so TypeScript doesn't strip the dependency. Without it,
  svelte-check sometimes warns; the runtime would still re-fire (it
  tracks reads on the proxy), but the warning is annoying.
- **The Saved row has no `database` field.** Don't add one — the table
  schema in M5.1 deliberately doesn't store it. The "resolve a sensible
  DB on open" responsibility lives entirely in `+page.svelte`.
- **`SavedQuery.scope` is a TS union (`"global" | "server"`)**, not a
  Rust enum on the wire. The M5.2 spec ensured `#[serde(rename = "scope")]`
  on `scope_str` produces a `scope` JSON key. If you see `scope_str` in
  the TS type, that means M5.2's serde rename is missing — go fix it.
- **`onauxclick` is not used here** (M5.3 only). The side panel uses
  double-click as its primary action; middle-click isn't bound.
- **`refresh` on subtab switch** is intentional. It means flipping
  History ↔ Saved re-fetches both lists across the switch — a tiny extra
  cost, but it keeps the lists fresh after long sessions without a
  background timer.
- **Inline rename `autofocus` warning.** svelte-check warns about
  `autofocus` for accessibility; silence with `<!-- svelte-ignore a11y_autofocus -->`
  immediately above the input. Same idiom as the password dialog.
- **`<dialog>` element vs the rest of the modal styling.** Each
  component in this task ships its own `.modal` CSS — slight duplication.
  Resist the urge to factor out a global modal mixin in M5.4; M6 polish
  is the right time to unify CSS.
- **Date formatting with `Intl.DateTimeFormat`** is preferable to manual
  padding, but the bespoke `formatTs` is ~15 lines and zero dependencies.
  Keep it.
- **Don't render N=200 history rows inside a `<table>`.** Each row is a
  `<button>` (so keyboard nav works in M6). The `<button>` reset CSS lives
  in `.row { ... border: none; ... }`. Verify in the smoke test that
  Tab-key navigation walks the list.
- **`shortSql` collapses internal whitespace.** A query like
  `"SELECT\n  *\nFROM users"` renders as `"SELECT * FROM users"`. The
  full text is available via the row's `title` attribute and via
  double-click → new tab.
- **Clear-history confirmation reuses a local modal.** Same shape as the
  ones in `+page.svelte`. Not a regression; just visually consistent.
- **No retention warning when the table fills up.** At 1000 rows the
  trim runs silently in M5.1's `history::append`. The side panel will
  show the newest 200 (`api.listHistory(200, ...)`); the user never sees
  the trim happen. M6 polish can add "history is at retention; older
  entries are pruned" if it matters.
- **Empty-state strings are different per subtab.** History: "No history
  yet." Saved: "No saved snippets yet. Click Save… on a tab to add one."
  The second nudges discoverability of the Save flow; the first doesn't
  need to.
- **`bind:this={saveDialog}` typed as `SaveDialog | undefined`** is the
  Svelte 5 pattern. Calling `saveDialog?.open()` is the public-method
  pattern; the component exports an `open(): void` function. Don't call
  `dialog.showModal()` from the parent — the dialog ref is internal.
- **Side panel resize.** v1 ships with the side panel at a fixed
  proportion: `min-height: 240px`, `flex: 1` on the connection tree
  block. No drag-to-resize. M6 polish.
- **`onError` toasts**. The parent stores one `sidePanelError` string and
  renders it below the panel. Multiple errors stack only as the latest;
  add a queue if M6 needs better visibility.

## Tests

`pnpm check` is the gate. No new automated tests.

### Manual smoke test

```bash
docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17
sqlite3 ~/.local/share/com.alberto.quill/quill.sqlite \
  "DELETE FROM query_history; DELETE FROM saved_queries;"
./run.sh
```

1. Connect to the local Postgres. Open `postgres`. **Side panel renders**
   below the tree with `History` and `Saved` sub-tabs. History list is
   empty.
2. Run `SELECT 1` in the active tab. Click `⟳` on the side panel.
   **History grows by one row.** Timestamp reads "Today HH:MM".
3. Run `SELECT 1/0`. **History grows by one failed row** in red with `✕`.
4. Click `⟳`. The most recent runs appear first.
5. Toggle the filter chip to "Current server only". List remains
   unchanged (single server). Connect to a second server (e.g. another
   docker container), run a query there, toggle filter back to "All
   servers" and confirm both servers' rows are visible.
6. Click `Save…` on the active tab. Dialog opens. Name = "demo". Choose
   "This server only". Save. Saved sub-tab refreshes (click `⟳`) and the
   row appears with a 🔒 icon.
7. Save another snippet as Global. Saved sub-tab shows both rows with
   🌐 and 🔒 icons.
8. Switch to the second server in the tree. **Global snippet still
   visible**; server-scoped is filtered out.
9. Switch back. Double-click the server-scoped row → **a new tab opens**
   pinned to the same `(server, database)` with the SQL pre-filled.
10. Double-click a history row → **new tab pinned to the original DB**
    (verify with `Change database…` showing the right one).
11. Rename the saved row inline (✎). Type a new name. Press Enter →
    commits. Press Escape on a rename in progress → cancels.
12. Delete a saved row (🗑). Row vanishes after refresh.
13. Try saving with a duplicate name in the same scope → **dialog
    surfaces "already exists"** inline; dialog stays open for retry.
14. Click "Clear history". Confirmation dialog. Confirm → list empties.
15. Disconnect the first server. The side panel doesn't touch it; the
    `Current server only` filter on history now shows no rows for that
    server (it filters by `server_id`, not by connection state).
16. Delete the first server via tree right-click → "Delete connection".
    Existing history rows cascade-delete via the FK. Saved rows scoped
    to that server also cascade. **Refresh the panel and verify.** Global
    saved snippets survive.

## Acceptance criteria

- [ ] `pnpm check` succeeds clean.
- [ ] `git status` shows two new files: `src/lib/SidePanel.svelte` and
      `src/lib/SaveDialog.svelte`.
- [ ] `grep -F "<SidePanel" src/routes/+page.svelte` matches once.
- [ ] `grep -F "<SaveDialog" src/routes/+page.svelte` matches once.
- [ ] `grep -F "api.listHistory\|api.clearHistory\|api.listSaved\|api.saveQuery\|api.deleteSaved\|api.renameSaved" src/lib/SidePanel.svelte src/lib/SaveDialog.svelte | wc -l`
      shows at least six call sites total.
- [ ] Smoke step 2 — successful query lands in history.
- [ ] Smoke step 3 — failed query lands in history in red.
- [ ] Smoke step 9 — server-scoped saved snippet opens with the original
      server pin.
- [ ] Smoke step 10 — history double-click opens a new tab pinned to the
      original DB.
- [ ] Smoke step 13 — duplicate save surfaces a friendly inline error.
- [ ] Smoke step 14 — Clear history empties the table after confirmation.
- [ ] Smoke step 16 — connection delete cascades to history + scoped
      saved rows; globals survive.
- [ ] No backend changes (`git diff src-tauri/` empty).
- [ ] No new pnpm dependencies.

## Out of scope

- Background auto-refresh of history / saved lists — explicitly forbidden
  per principle 1.
- Server-scope filter on the Saved list beyond global+selected — M6
  polish if it matters.
- Tags / folders on saved snippets — v1.1.
- Drag a saved snippet onto a tab → "insert" — v1.1.
- Search/filter inside the History panel — v1.1.
- Undo for Delete saved — v1.1.
- Keyboard navigation (Tab to walk rows, Enter to open) — M6 polish.
- Resize handle between tree and side panel — M6 polish.
- Surfacing retention prune events to the user — M6.
- Cross-server tab move on Saved double-click — out of scope; user
  picks the server first via the tree.
- Multi-line SQL preview / syntax highlight in the list — v1.1.
- Tooltips with full SQL beyond `title="..."` — M6 polish.
- A "Run from history" shortcut that bypasses the editor — out of
  scope; double-click → new tab → user presses Cmd+Enter is the v1 path.
