<script lang="ts">
  //! Result strip below the editor.  One chip per kept result pane; pure
  //! presentation, events bubble to the parent which owns the pane list.
  //! A tab keeps at most one unpinned ("scratch") pane — pinning snapshots a
  //! pane so it survives the next run.

  import type { ResultPane } from "./tabs";

  let {
    panes,
    activeId,
    onSelect,
    onClose,
    onTogglePin,
  }: {
    panes: ResultPane[];
    activeId: number | null;
    onSelect: (paneId: number) => void;
    onClose: (paneId: number) => void;
    onTogglePin: (paneId: number) => void;
  } = $props();

  function label(p: ResultPane): string {
    return `${p.rowCount.toLocaleString()} rows · ${p.durationMs}ms`;
  }

  function tooltip(p: ResultPane): string {
    const lines = [p.sql.trim()];
    lines.push(p.pinned ? "Pinned — click 📌 to unpin" : "Click 📌 to pin (keeps this result on re-run)");
    return lines.join("\n\n");
  }
</script>

{#if panes.length > 0}
  <div class="result-strip" role="tablist">
    {#each panes as p (p.paneId)}
      {@const isActive = p.paneId === activeId}
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions a11y_click_events_have_key_events -->
      <div
        class="chip"
        class:active={isActive}
        class:pinned={p.pinned}
        role="tab"
        tabindex={isActive ? 0 : -1}
        aria-selected={isActive}
        onclick={() => onSelect(p.paneId)}
        onauxclick={(e) => { if (e.button === 1) { e.preventDefault(); onClose(p.paneId); } }}
        onkeydown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onSelect(p.paneId); } }}
        title={tooltip(p)}
      >
        <button
          class="pin"
          class:on={p.pinned}
          aria-label={p.pinned ? "Unpin result" : "Pin result"}
          aria-pressed={p.pinned}
          onclick={(e) => { e.stopPropagation(); onTogglePin(p.paneId); }}
        >📌</button>
        <span class="text">{label(p)}</span>
        {#if p.hasMore}<span class="cursor" aria-label="cursor open" title="cursor open">●</span>{/if}
        <button
          class="close"
          aria-label="Close result"
          onclick={(e) => { e.stopPropagation(); onClose(p.paneId); }}
        >×</button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .result-strip {
    display: flex;
    gap: 0;
    border-bottom: 1px solid var(--border-primary);
    background: var(--bg-tertiary);
    align-items: stretch;
    overflow-x: auto;
  }
  .chip {
    display: flex;
    align-items: center;
    gap: 0.15rem;
    padding: 0.15rem 0.4rem;
    border-right: 1px solid var(--border-light);
    border-bottom: 2px solid transparent;
    cursor: pointer;
    user-select: none;
    font-size: 0.78rem;
    white-space: nowrap;
  }
  .chip:hover { background: var(--bg-hover); }
  .chip.active { background: var(--bg-surface); border-bottom-color: var(--text-accent); }

  .text {
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 12rem;
  }
  .chip.active .text { color: var(--text-primary); }

  .cursor { color: var(--text-orange); font-size: 0.6rem; line-height: 1; }

  .pin {
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 0.78rem;
    line-height: 1;
    padding: 0 0.1rem;
    /* Unpinned: faint, desaturated.  Pinned: full colour. */
    filter: grayscale(1);
    opacity: 0.4;
  }
  .pin:hover { opacity: 0.8; }
  .pin.on { filter: none; opacity: 1; }

  .close {
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 0.9rem;
    line-height: 1;
    padding: 0 0.15rem;
    color: var(--text-muted);
  }
  .close:hover { color: var(--text-error); background: var(--close-hover-bg); border-radius: 2px; }
</style>
