<script lang="ts">
  //! Side panel with two sub-tabs: History (executed queries) and Saved
  //! (named snippets).  Loads on mount and on explicit refresh; never
  //! polls.

  import { api, type HistoryRecord, type SavedQuery, type SavedScope } from "./tauri";
  import { errorMessage } from "./tree";

  let {
    selectedServerId,
    resolveDb,
    onOpenInNewTab,
    onError,
  }: {
    selectedServerId: number | null;
    resolveDb: (serverId: number) => string;
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
    const sid = record.server_id ?? selectedServerId;
    if (sid === null) {
      onError("Select a server in the tree before opening a global snippet.");
      return;
    }
    // Saved snippets are db-agnostic; resolve to the server's default DB.
    onOpenInNewTab(sid, resolveDb(sid), record.sql);
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

  .modal { border: 1px solid #888; border-radius: 8px; padding: 1.25rem; max-width: 360px; }
  .modal::backdrop { background: rgba(0,0,0,0.3); }
  .modal-actions { display: flex; justify-content: flex-end; gap: 0.5rem; margin-top: 0.75rem; }
  .btn { padding: 0.3rem 0.6rem; border: 1px solid #888; border-radius: 4px; background: #f0f0f0; cursor: pointer; font: inherit; font-size: 0.9rem; }
  .btn:hover { background: #e0e0e0; }
  .btn-danger { background: #b00020; color: white; border-color: #8a0019; }
  .btn-danger:hover { background: #8a0019; }
</style>
