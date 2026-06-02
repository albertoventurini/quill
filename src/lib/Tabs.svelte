<script lang="ts">
  //! Tab strip above the editor.  Pure presentation: events bubble up to
  //! the parent, which owns the tab list.

  import type { Tab } from "./tabs";
  import { serverColor } from "./serverColor";

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

  /** Full path for the hover tooltip — the label itself only shows the
   *  capped `db.schema`, so the tooltip carries the complete context. */
  function tabTitle(t: Tab): string {
    if (t.kind === "erd") {
      return [
        `${serverNameLookup(t.serverId)} / ${t.database} . ${t.erd?.schema ?? ""}`,
        "Entity-relationship diagram",
      ].join("\n");
    }
    const path = t.schema
      ? `${serverNameLookup(t.serverId)} / ${t.database} . ${t.schema}`
      : `${serverNameLookup(t.serverId)} / ${t.database}`;
    const lines = [path];
    if (t.schema) lines.push(`search_path → ${t.schema}`);
    lines.push("Right-click to change database");
    return lines.join("\n");
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
      style="--server-color: {serverColor(t.serverId)}"
      onclick={() => onSelect(t.id)}
      onauxclick={(e) => onMiddleClick(e, t.id)}
      oncontextmenu={(e) => onTabContextMenu(e, t.id)}
      onkeydown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onSelect(t.id); } }}
      title={tabTitle(t)}
    >
      <span class="accent" aria-hidden="true"></span>
      <span class="labels">
        <span class="server">{serverNameLookup(t.serverId)}</span>
        <span class="path">
          {#if t.kind === "erd"}
            <span class="path-text"><span class="erd-badge">ERD</span><span class="schema">{t.erd?.schema}</span></span>
          {:else}
            <span class="path-text"><span class="db">{t.database}</span>{#if t.schema}<span class="sep">.</span><span class="schema">{t.schema}</span>{/if}</span>
            {#if dirty(t)}<span class="dirty" aria-label="unsaved">•</span>{/if}
          {/if}
        </span>
      </span>
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
    align-items: stretch;
    border-right: 1px solid var(--border-light);
    border-bottom: 2px solid transparent;
    cursor: pointer;
    user-select: none;
  }
  .tab:hover { background: var(--bg-hover); }
  .tab.active { background: var(--bg-surface); border-bottom-color: var(--text-accent); }

  /* Per-server colour bar (see serverColor.ts). */
  .accent {
    flex: none;
    width: 3px;
    align-self: stretch;
    background: var(--server-color, transparent);
  }

  .labels {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 0.05rem;
    padding: 0.2rem 0.5rem;
    min-width: 0;
    max-width: 15rem;
  }
  .server {
    font-size: 0.68rem;
    line-height: 1.15;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .path {
    display: flex;
    align-items: center;
    min-width: 0;
    font-size: 0.8rem;
    line-height: 1.2;
  }
  .path-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sep { color: var(--text-faint); }
  .schema { color: var(--text-accent); font-weight: 500; }
  .erd-badge {
    font-size: 0.6rem;
    font-weight: 700;
    letter-spacing: 0.03em;
    color: var(--text-accent);
    border: 1px solid var(--border-secondary);
    border-radius: 2px;
    padding: 0 0.2rem;
    margin-right: 0.3rem;
  }
  .db { color: var(--text-primary); font-weight: 500; }
  /* Muted = the tab matches the tree's current selection. */
  .tab.muted .server { color: var(--text-faint); }
  .tab.muted .db { color: var(--text-muted); }
  .tab:not(.muted) .db { color: var(--text-orange); font-weight: 600; }
  .dirty { flex: none; color: var(--text-orange); padding-left: 0.2rem; }
  .close {
    align-self: center;
    margin: 0 0.3rem 0 0.1rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 0.95rem;
    line-height: 1;
    padding: 0 0.2rem;
    color: var(--text-muted);
  }
  .close:hover { color: var(--text-error); background: var(--close-hover-bg); border-radius: 2px; }
  .add {
    padding: 0 0.6rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 1rem;
    color: var(--text-mid);
  }
  .add:hover { background: var(--bg-hover); color: var(--text-primary); }
</style>
