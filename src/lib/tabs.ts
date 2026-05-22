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
