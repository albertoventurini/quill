# M3.6 — Result grid + UI wiring (Cancel, errors, Load more)

## Goal

**Before (post-M3.5):** The right pane has a real CodeMirror editor, but the result is still rendered as a tab-separated string inside a `<pre>` block. There's no Load-more button (M3.4's `fetch_more` is unused from the UI). There's no Cancel button (M3.3's `cancel_query` is unused from the UI). Errors are rendered as an unformatted `<pre>` with `[Kind] message`. `result_id` is captured in JS but never used.

**After:** A new `src/lib/ResultGrid.svelte` renders columns + rows in a real HTML table with:
- **Sortable** column headers (single column at a time, ascending / descending toggle, stable secondary sort by row order).
- **Resizable** columns (drag the right edge of a header).
- **Cell preview** dialog for long values (click a cell with >120 chars or any non-string `object` to open a `<dialog>` showing the raw text).
- **Read-only** — no inline editing hooks anywhere.
- A **status line** above the grid: `5,000 rows · 217ms · slot [1/2] · postgres@local · cursor open`.
- A **Load more** button below the grid when `has_more`; click → `api.fetchMore(resultId)` → append to the materialized rows.
- A **Cancel** button next to Run, visible only while `runningQuery` *or* while a cursor is open. Clicking → `api.cancelQuery(serverId, database)`. Cancel during `run_query` causes the Promise to reject → an inline error renders. Cancel during a `fetch_more` causes the next chunk fetch to fail and the result is auto-closed.
- A **Close result** affordance (the "Close result" button next to Cancel) that calls `api.closeResult(resultId)` and clears the grid. Switching the active DB (clicking a different DB in the tree) also auto-closes any open result.
- A proper **inline error** region between the editor and the grid: error rendered as a single-line "X" badge + message; no `<pre>` shell shape.

This task is **frontend only** plus a small bridge update.

## Current state

### `src/routes/+page.svelte` (post-M3.5)

- Editor in place via `<Editor>`.
- `result: $state<RunResult | { error: CommandError } | null>` rendered as `<pre>{renderResult(result)}</pre>`.
- `runFromEditor` calls `api.runQuery` and writes `result`.
- `editorWarning` is the only inline warning surface (for multi-statement and empty-buffer).
- The right-pane DB heading is `{selectedConn.name} / {selectedDb.database}`.

### `src/lib/tauri.ts` (post-M3.4)

`runQuery` returns `RunResult`. `fetchMore` and `closeResult` already exist. `cancelQuery` exists. `getSlotState` exists.

### `src/lib/Tree.svelte`, `tree.ts`, `Editor.svelte`, `statement.ts`

Unchanged in this task.

## Design choices baked into this spec

- **Grid is a real `<table>` inside a scrollable container.** No virtualization (PRD §9 — "no heavy component framework"). For >50k rows the user is expected to add LIMIT. Performance budget: 10k rows render in <100ms; tested informally during smoke.
- **Column resizing via a hand-rolled drag handle on each `<th>`.** ~30 lines of pointermove + pointerup logic. Widths stored in `widths: $state<number[]>` keyed by column index. Default = `auto` (let the table compute it on first render, then freeze).
- **Sorting is client-side over the materialized rows.** When the user clicks a header, sort the `rows` array by that column's value (string compare with `Intl.Collator` for stability). Click again to flip; click a third time to clear (return to insertion order). No multi-column sort; no per-type type-aware comparators in v1 (everything sorts as string).
- **Sorting interacts with Load-more.** New chunks append in insertion order; if a sort is active, the appended rows re-sort. Document this — users may be surprised.
- **Cell preview dialog.** A `<dialog>` element with the raw cell value. Triggered by clicking a cell whose rendered string exceeds 120 chars, **or** any non-null `object` (JSONB / arrays render as JSON.stringify). Click outside / Esc / Close button dismisses.
- **Cancel button.** Always-on while `runningQuery=true` *or* while `result?.result_id` is set (cursor open). Disabled otherwise. Fires `api.cancelQuery(serverId, database)` — passes `database` so introspection slots on other DBs aren't affected.
- **Close-result on DB change.** If a result is open and the user clicks a different DB in the tree, the active result is closed via `api.closeResult`. The grid clears. This avoids a subtle leak where the user opens a result, switches DBs, runs a new query — and the old result stays open holding a slot for the previous DB.
- **Close-result on disconnect.** Backend `disconnect_server` (M3.4) already sweeps. The frontend just needs to clear `result` and `resultId` when `connectedState[serverId]` becomes absent.
- **Status line is informational, not interactive.** Slot indicator is a separate badge in the tree; the status line just shows the snapshot at this query's run-time.
- **`result_id` is opaque to the frontend.** Stored as `string`. Always paired with the server/DB it belongs to.

## Deliverables

### 1. `src/lib/ResultGrid.svelte` — new component

```svelte
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
```

### 2. `src/routes/+page.svelte` — rewire result/Cancel/Load-more

Add imports:

```ts
import ResultGrid from "$lib/ResultGrid.svelte";
import type { RunResult, ChunkResult } from "$lib/tauri";
```

Replace the `result` shape with a richer materialized model:

```ts
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
```

Remove the old `result` state and `renderResult` function.

Update `runFromEditor` to close any existing result first, then populate `active`:

```ts
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

  // Close any prior result before launching a new one.
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
      // Backend auto-closed; reflect that here.
      active.resultId = ""; // mark closed
    }
  } catch (err) {
    lastError = err as CommandError;
    // Backend already removed the result entry on fetch error.
    active = null;
  } finally {
    loadingMore = false;
  }
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
}

async function cancelRunning() {
  if (!selectedDb) return;
  try {
    await api.cancelQuery(selectedDb.serverId, selectedDb.database);
  } catch (err) {
    lastError = err as CommandError;
  }
}
```

Update the disconnect handler:

```ts
async function disconnect(id: number) {
  if (active && selectedDb?.serverId === id) {
    active = null; // backend sweep handles the close
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
```

Update `selectDb` to close a result when switching DBs:

```ts
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
```

Update the right pane markup:

```svelte
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
```

Status-line helper:

```ts
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
```

Refresh slot snapshots after run / load-more / cancel so the status line reflects current `[busy/budget]`:

```ts
async function refreshSlotState(serverId: number) {
  const s = await api.getSlotState(serverId);
  if (s) connectedState[serverId] = s;
}
```

Call it at the end of `runFromEditor`, `loadMore`, `cancelRunning`, and `closeActive`. The slot indicator on the tree also picks up the change since it reads from `connectedState`.

Add minimal styles:

```css
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
```

### 3. (Optional polish) Auto-refresh slot indicator after Cancel

After `cancelRunning` resolves, fire `refreshSlotState(selectedDb.serverId)` so the tree's `[busy/budget]` returns to `[0/budget]` immediately rather than waiting for the next user action.

## Implementation order

1. **`src/lib/ResultGrid.svelte`** — write the component in isolation. Optional dummy-data preview by importing into a throwaway `+page.svelte` while iterating; revert before commit.
2. **`src/routes/+page.svelte`** — rewire state. Order of edits:
   1. Add the new state vars (`active`, `lastError`, `loadingMore`).
   2. Replace `runFromEditor` body.
   3. Add `loadMore`, `closeActive`, `cancelRunning`, `refreshSlotState`, `statusLineText`.
   4. Replace the right-pane markup.
   5. Update `disconnect` and `selectDb` to close active result.
3. `pnpm check` — must pass clean.
4. Smoke test below.

## Known gotchas

- **`{#each ... (row)}` keying.** Using the row array reference as the key works because Svelte 5 uses `===` for keys and array literals are stable across re-renders (we don't replace, we append). If you ever switch to spreading rows into a new array on every change, switch to an explicit `(index)` key.
- **Resize handle uses `pointerdown` + window listeners.** Don't try `mousedown`; touch-screen Tauri builds may exist. `pointerup` on the window covers cases where the cursor leaves the th.
- **Cell click vs sort header click.** Sort fires on `<button>` inside `<th>`. Resize fires on `<span.resize-handle>` whose `pointerdown` prevents the click from bubbling to the header — verify in the smoke test (drag-resize should NOT trigger a sort).
- **`<dialog>` API.** Tauri 2's webview supports the native `<dialog>` element. `showModal()` opens; submitting any form inside with `method="dialog"` closes. Esc closes. Clicking outside does NOT close — that's a deliberate omission, browsers don't ship that natively.
- **`Intl.Collator` with `{ numeric: true }`** sorts `"10"` after `"2"` correctly. Use it for the single comparator — type-aware sort (treat INT4 as numeric, TEXT as string) is M6 polish.
- **Sort + Load-more interaction.** Each new chunk reorders; the user may see new rows pop into the middle of the sort. Documented; no auto-clear of sort.
- **`Cancel` button while idle.** Disabled. Pressing it with no in-flight query is a no-op anyway (`cancelled: 0`), but the disabled state matches user expectation.
- **`closeResult` may race with `fetch_more`.** If the user clicks Close while a fetch is mid-flight, the dashmap entry is removed; the fetch's await on the cursor returns an error; `loadMore` catches it; `active` is already null. No corruption.
- **Switching DBs closes the result.** This is intentional. If the user wants to keep the result open, they should not click another DB. The decision is per AGENTS.md principle 2 (don't leak slots).
- **Disconnect closes everything.** The backend `disconnect_server` sweeps the registry (M3.4). The frontend just nulls `active`. If the disconnect happens via the tree (right-click → Disconnect), `disconnect(id)` runs the sweep server-side.
- **Status line dependency on `connectedState`.** When the slot transitions busy → idle (after `closeActive`), call `refreshSlotState(serverId)` so the status line and tree badge agree. Skipping this leaves `[1/2]` stale until the next user action.
- **Per-row `<td>` `onclick` for preview.** If a row has many short string cells and one long one, only the long one is clickable (we check `shouldPreview(v)`). A cursor-zoom hint visualizes which cells are clickable.
- **Cancel propagation.** When `cancel_query` succeeds against an in-flight `run_query`, the rejection arrives in `runFromEditor`'s `catch`. `lastError.message` typically reads "ERROR: canceling statement due to user request" — render as-is. The badge says `PG`.
- **No spinner inside the grid for loading-more.** The button text flips to "Loading…" and disables. Good enough for v1.
- **No keyboard navigation in the grid.** Arrow keys don't traverse cells. M6 polish.
- **A11y warnings about `<span role="separator">`.** svelte-check may warn about missing `tabindex`. Suppress with `<!-- svelte-ignore a11y_no_static_element_interactions -->` on the line — the resize handle is fundamentally pointer-driven; M6 polish adds keyboard equivalents.

## Tests

`pnpm check` must pass. No automated tests are added (Vitest isn't wired up).

### Manual smoke test

Run the fixture:

```bash
docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17
docker exec -it quill-pg psql -U postgres -c "
  CREATE TABLE big AS SELECT generate_series(1, 5000) AS n, md5(random()::text) AS hash;
"
./run.sh
```

1. Connect to the local Postgres. Open the `postgres` DB.
2. Run `SELECT * FROM big`. The grid shows 1000 rows. Status line reads `1,000 rows · Nms · slot [1/2] · local@postgres · cursor open`. Tree slot indicator: `[1/2]`.
3. Click the `n` column header — rows sort ascending (▲ icon). Click again — descending (▼). Click third time — original order.
4. Click the `hash` column header — sorts by hex string.
5. Drag the right edge of the `hash` header to make it wider. The sort header is **not** triggered by the drag (verify).
6. Click a `hash` cell — preview dialog opens with the full text. Press Esc to close.
7. Click **Load more**. The grid grows to 2000 rows; status updates; slot stays `[1/2]`.
8. Click **Load more** until exhausted. Final state: 5000 rows, `has_more=false`, status reads `cursor closed`, slot drops to `[0/2]`. **Tree slot indicator updates** to `[0/2]`.
9. Run `SELECT pg_sleep(10), 1`. While the spinner is up, click **Cancel**. The query rejects within ~1s; inline error shows `[PG] ERROR: canceling statement due to user request`. Slot drops to `[0/2]`.
10. Run `SELECT * FROM big` again. Slot bumps to `[1/2]`. Click a different DB in the tree (e.g. switch to `template1` after connecting). The grid clears; the previous result is closed (verify slot `[0/2]`, no leftover cursor).
11. Run a query that errors: `SELECT FROM nope`. Inline error renders with `[PG]` badge; no grid is shown; no slot stays busy.
12. Run a tall narrow result: `SELECT generate_series(1, 1000)`. One column, 1000 rows visible after first chunk (chunk_size=1000), `cursor closed` because `has_more = false`.
13. Run a wide result: `SELECT *, repeat('x', 200) AS big_field FROM big LIMIT 10`. The `big_field` cell is truncated with `…`. Click → preview dialog shows full 200-char string.
14. Run a JSON result: `SELECT row_to_json(big) AS doc FROM big LIMIT 3`. JSONB cells render as compact JSON and are clickable for preview.
15. Disconnect the server. The grid clears (no leftover state).

## Acceptance criteria

- [ ] `pnpm check` succeeds.
- [ ] `git status -- src/lib/` shows `ResultGrid.svelte` added.
- [ ] `grep -F '<pre' src/routes/+page.svelte` returns zero matches in result-render code.
- [ ] `grep -F 'cancelQuery' src/routes/+page.svelte` returns at least one match (the Cancel button handler).
- [ ] `grep -F 'fetchMore' src/routes/+page.svelte` returns at least one match.
- [ ] `grep -F 'closeResult' src/routes/+page.svelte` returns at least one match.
- [ ] `grep -E "on:click|on:submit|on:pointerdown|on:keydown" src/lib/ResultGrid.svelte src/routes/+page.svelte` returns zero matches (Svelte 5 syntax only).
- [ ] Smoke step 3 — sort tri-state works.
- [ ] Smoke step 5 — resize doesn't trigger sort.
- [ ] Smoke step 7 — Load-more appends and slot stays `[1/2]`.
- [ ] Smoke step 8 — exhaustion auto-closes and slot returns to `[0/2]` (frontend slot badge updates).
- [ ] Smoke step 9 — Cancel rejects the in-flight query, inline error visible.
- [ ] Smoke step 10 — DB switch closes the prior result.
- [ ] Smoke step 11 — error path doesn't leak a slot.
- [ ] Smoke step 13 — cell preview dialog renders long values.
- [ ] No backend changes; `git diff src-tauri/` is empty for this task.
- [ ] No new pnpm deps in this task.

## Out of scope

- CSV export — **M5**.
- Tabs / query history / saved queries — **M5**.
- Dark mode for the grid — **M6**.
- Keyboard navigation inside the grid — **M6**.
- Per-type sort comparators (numeric vs string vs date) — **M6**.
- Multi-column sort — v1.1.
- Drag-to-reorder columns — v1.1.
- Persisting column widths across sessions — **M6** (settings).
- Tooltip on `[busy/budget]` showing each slot's database — **M6** polish.
- Auto-cancel running query when switching DBs — explicitly **not** in scope; the user clicks Cancel themselves if they want.
- Showing `CancelOutcome.errors` from M3.3 — currently silently ignored; **M6** polish.
- Time-series visualization, ER diagrams, plan visualization — PRD §3 non-goals.
- Editable cells — PRD §3 non-goal.
