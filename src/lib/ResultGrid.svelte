<script lang="ts">
  //! Read-only sortable + resizable table grid for query results.
  //!
  //! Owns its own sort state and column widths; the materialized row
  //! buffer is passed in by the parent (which appends on Load more).

  import type { ColumnMeta } from "$lib/tauri";

  let {
    columns,
    rows,
    statusLine,
    hasMore,
    loadingMore,
    onLoadMore,
    canLoadMore,
  }: {
    columns: ColumnMeta[];
    rows: unknown[][];
    statusLine: string;
    hasMore: boolean;
    loadingMore: boolean;
    onLoadMore: () => void;
    canLoadMore: boolean;
  } = $props();

  // ── Sort state (single-column tri-state: asc / desc / none) ──

  type SortDir = "asc" | "desc" | null;

  let sortCol = $state<number | null>(null);
  let sortDir = $state<SortDir>(null);

  function clickSort(col: number) {
    if (sortCol !== col) {
      sortCol = col;
      sortDir = "asc";
    } else if (sortDir === "asc") {
      sortDir = "desc";
    } else if (sortDir === "desc") {
      sortCol = null;
      sortDir = null;
    } else {
      sortDir = "asc";
    }
  }

  const collator = new Intl.Collator(undefined, { numeric: true });

  let sortedRows = $derived.by(() => {
    if (sortCol === null || sortDir === null) return rows;
    const idx = sortCol;
    const factor = sortDir === "asc" ? 1 : -1;
    return [...rows]
      .map((r, originalIndex) => ({ r, originalIndex }))
      .sort((a, b) => {
        const av = stringify(a.r[idx]);
        const bv = stringify(b.r[idx]);
        const cmp = collator.compare(av, bv);
        return cmp === 0 ? a.originalIndex - b.originalIndex : cmp * factor;
      })
      .map((x) => x.r);
  });

  // ── Column widths (in px; null = let the browser compute) ──

  let widths = $state<(number | null)[]>([]);
  $effect(() => {
    // When columns change shape, reset widths.
    if (widths.length !== columns.length) {
      widths = columns.map(() => null);
    }
  });

  function startResize(col: number, e: PointerEvent) {
    e.preventDefault();
    const th = (e.currentTarget as HTMLElement).closest("th");
    if (!th) return;
    const startX = e.clientX;
    const startW = th.getBoundingClientRect().width;

    const move = (ev: PointerEvent) => {
      const next = Math.max(40, startW + (ev.clientX - startX));
      widths[col] = next;
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  // ── Cell preview dialog ──

  let previewDialog = $state<HTMLDialogElement | null>(null);
  let previewText = $state("");

  function openPreview(value: unknown) {
    previewText = stringify(value);
    previewDialog?.showModal();
  }

  function stringify(value: unknown): string {
    if (value === null) return "NULL";
    if (typeof value === "string") return value;
    if (typeof value === "object") {
      try {
        return JSON.stringify(value, null, 2);
      } catch {
        return String(value);
      }
    }
    return String(value);
  }

  function shouldPreview(value: unknown): boolean {
    if (value === null) return false;
    if (typeof value === "object") return true;
    return stringify(value).length > 120;
  }

  function cellDisplay(value: unknown): string {
    const s = stringify(value);
    if (s.length > 120) return s.slice(0, 117) + "…";
    return s;
  }

  function sortIcon(col: number): string {
    if (sortCol !== col) return "";
    return sortDir === "asc" ? " ▲" : sortDir === "desc" ? " ▼" : "";
  }
</script>

<div class="status-line">{statusLine}</div>

<div class="grid-scroll">
  <table>
    <thead>
      <tr>
        {#each columns as col, i (i)}
          <th style={widths[i] != null ? `width:${widths[i]}px` : ""}>
            <button
              type="button"
              class="header-button"
              onclick={() => clickSort(i)}
              title="Click to sort"
            >
              <span>{col.name}</span>
              <span class="type">{col.type_name}</span>
              <span class="sort-icon">{sortIcon(i)}</span>
            </button>
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <span
              class="resize-handle"
              role="separator"
              aria-orientation="vertical"
              onpointerdown={(e) => startResize(i, e)}
            ></span>
          </th>
        {/each}
      </tr>
    </thead>
    <tbody>
      {#each sortedRows as row (row)}
        <tr>
          {#each columns as _, i (i)}
            {@const v = row[i]}
            <td
              class:nullable={v === null}
              class:previewable={shouldPreview(v)}
              onclick={() => shouldPreview(v) && openPreview(v)}
            >
              {cellDisplay(v)}
            </td>
          {/each}
        </tr>
      {/each}
    </tbody>
  </table>
</div>

{#if hasMore}
  <div class="load-more-row">
    <button class="btn" onclick={onLoadMore} disabled={!canLoadMore || loadingMore}>
      {loadingMore ? "Loading…" : "Load more"}
    </button>
  </div>
{/if}

<dialog bind:this={previewDialog} class="preview-dialog">
  <pre>{previewText}</pre>
  <form method="dialog" class="preview-actions">
    <button class="btn">Close</button>
  </form>
</dialog>

<style>
  .status-line {
    padding: 0.4rem 0.6rem;
    font-size: 0.85rem;
    color: #444;
    border-bottom: 1px solid #ddd;
    background: #f7f7f7;
    font-variant-numeric: tabular-nums;
  }
  .grid-scroll {
    flex: 1;
    overflow: auto;
    border: 1px solid #ccc;
    border-radius: 4px;
    background: white;
  }
  table {
    border-collapse: collapse;
    font-size: 13px;
    width: max-content;
    min-width: 100%;
  }
  th {
    position: sticky;
    top: 0;
    background: #f0f0f0;
    border-bottom: 1px solid #aaa;
    border-right: 1px solid #ddd;
    padding: 0;
    text-align: left;
    white-space: nowrap;
  }
  .header-button {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    width: 100%;
    padding: 0.35rem 0.6rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font: inherit;
    font-size: 0.85rem;
    text-align: left;
  }
  .header-button:hover {
    background: #e6e6e6;
  }
  .type {
    color: #888;
    font-weight: normal;
    font-size: 0.75rem;
  }
  .sort-icon {
    margin-left: auto;
    color: #444;
  }
  .resize-handle {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    width: 5px;
    cursor: col-resize;
    background: transparent;
  }
  .resize-handle:hover {
    background: #3366cc;
  }
  td {
    padding: 0.25rem 0.6rem;
    border-bottom: 1px solid #f0f0f0;
    border-right: 1px solid #f0f0f0;
    white-space: nowrap;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    max-width: 480px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  td.nullable {
    color: #888;
    font-style: italic;
  }
  td.previewable {
    cursor: zoom-in;
  }
  td.previewable:hover {
    background: #f0f0ff;
  }
  .load-more-row {
    padding: 0.4rem 0;
    display: flex;
    justify-content: center;
  }
  .preview-dialog {
    max-width: 80vw;
    max-height: 80vh;
    padding: 0;
    border: 1px solid #888;
    border-radius: 6px;
  }
  .preview-dialog pre {
    margin: 0;
    padding: 1rem;
    max-height: 60vh;
    overflow: auto;
    font: 13px ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .preview-actions {
    display: flex;
    justify-content: flex-end;
    padding: 0.5rem;
    border-top: 1px solid #eee;
  }
</style>
