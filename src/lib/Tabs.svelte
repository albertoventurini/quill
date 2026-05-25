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
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions a11y_click_events_have_key_events -->
    <div
      class="tab"
      class:active={isActive}
      class:muted
      role="tab"
      tabindex={isActive ? 0 : -1}
      aria-selected={isActive}
      onclick={() => onSelect(t.id)}
      onauxclick={(e) => onMiddleClick(e, t.id)}
      oncontextmenu={(e) => onTabContextMenu(e, t.id)}
      onkeydown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onSelect(t.id); } }}
      title="Right-click to change database"
    >
      <span class="server">{serverNameLookup(t.serverId)}</span>
      <span class="sep">/</span>
      <span class="db">{t.database}</span>
      {#if t.schema}<span class="sep">.</span><span class="schema" title="search_path scoped to {t.schema}">{t.schema}</span>{/if}
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
    border-bottom: 1px solid var(--border-primary);
    background: var(--bg-tertiary);
    align-items: stretch;
    overflow-x: auto;
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    padding: 0.35rem 0.6rem;
    border-right: 1px solid var(--border-light);
    cursor: pointer;
    font-size: 0.85rem;
    user-select: none;
    white-space: nowrap;
  }
  .tab:hover { background: var(--bg-hover); }
  .tab.active { background: var(--bg-surface); border-bottom: 2px solid var(--text-accent); }
  .server { color: var(--text-primary); font-weight: 500; }
  .sep { color: var(--text-faint); }
  .schema { color: var(--text-accent); font-weight: 500; }
  /* Muted = the tab matches the tree's current selection. */
  .tab.muted .server, .tab.muted .db { color: var(--text-muted); }
  .tab:not(.muted) .db { color: var(--text-orange); font-weight: 600; }
  .dirty { color: var(--text-orange); padding-left: 0.15rem; }
  .close {
    margin-left: 0.4rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 1rem;
    line-height: 1;
    padding: 0 0.25rem;
    color: var(--text-muted);
  }
  .close:hover { color: var(--text-error); background: var(--close-hover-bg); border-radius: 2px; }
  .add {
    padding: 0.35rem 0.6rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 1rem;
    color: var(--text-mid);
  }
  .add:hover { background: var(--bg-hover); color: var(--text-primary); }
</style>
