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

/** One result kept in a query tab's result strip.  A tab holds at most one
 *  *unpinned* pane (the "scratch" result, replaced in place on each run); the
 *  scratch pane is also the only one that may hold a live cursor/slot.  Pinning
 *  snapshots the pane — its cursor is closed (slot freed) and `resultId` cleared
 *  — so pinned panes survive re-runs without consuming the connection budget. */
export type ResultPane = ActiveResult & {
  /** Monotonic; assigned at creation.  Identifies the pane within its tab. */
  paneId: number;

  /** Pinned panes are kept across re-runs; the unpinned scratch pane is
   *  replaced.  Pinning a pane snapshots it (closes its cursor). */
  pinned: boolean;

  /** The statement that produced this result — shown as the pane's tooltip so
   *  same-sized results stay distinguishable. */
  sql: string;
};

/** What a tab displays. "query" is the editor + result grid; "erd" is an
 *  entity-relationship diagram of a schema (or a subset of its tables). */
export type TabKind = "query" | "erd";

/** State for an ERD tab.  `seed` is the initial table selection — either an
 *  explicit list or "all" (every relation in the schema). */
export type ErdState = {
  schema: string;
  seed: string[] | "all";
  includeNeighbors: boolean;
};

export type Tab = {
  /** Monotonic; assigned at creation. */
  id: number;

  /** What this tab shows.  Defaults to "query". */
  kind: TabKind;

  /** Pin: tab targets this server's database, period.  Mutated only via
   *  the explicit "Change database…" action; never by tree selection. */
  serverId: number;
  database: string;

  /** When set, the tab was opened against a schema node; every run sends
   *  `SET LOCAL search_path TO "<schema>"` so unqualified names resolve in
   *  this schema only.  `null` = no scoping (database-level editor). */
  schema: string | null;

  /** Current editor buffer.  `<Editor>` is the source of truth while
   *  mounted; this is the value persisted across un-/re-mount. */
  sql: string;

  /** Snapshot of `sql` at tab creation (or last Save).  `dirty` is
   *  computed as `sql !== initialSql`. */
  initialSql: string;

  /** Result panes shown in this tab's result strip, oldest first.  Empty until
   *  the first successful `run_query`.  At most one pane is unpinned (scratch);
   *  see {@link ResultPane}. */
  results: ResultPane[];

  /** `paneId` of the currently-displayed pane, or `null` when `results` is
   *  empty.  Resolve via {@link activeResult}. */
  activeResultId: number | null;

  /** Inline error from the last Run / Load more / Cancel. */
  lastError: CommandError | null;

  /** Warning above the inline error (multi-statement, empty buffer, etc.) */
  editorWarning: string | null;

  /** Set while a `run_query` is in flight. */
  runningQuery: boolean;

  /** Set while a `fetch_more` is in flight. */
  loadingMore: boolean;

  /** Present only when `kind === "erd"`; the diagram's scope and options. */
  erd: ErdState | null;
};

let nextId = 1;
let nextPaneId = 1;

/** Build a fresh result pane from a `run_query` response.  `pinned` starts
 *  false (it's the scratch pane until the user pins it). */
export function makeResultPane(
  sql: string,
  result: ActiveResult,
): ResultPane {
  return { ...result, paneId: nextPaneId++, pinned: false, sql };
}

/** The pane currently displayed in `tab`, or `null` when the tab has no
 *  results. */
export function activeResult(tab: Tab): ResultPane | null {
  return tab.results.find((r) => r.paneId === tab.activeResultId) ?? null;
}

/** Create a new query tab pinned to `(serverId, database)` with an empty
 *  buffer (or the supplied `sql`).  `initialSql` is set to the same value so
 *  the tab starts non-dirty. */
export function makeTab(
  serverId: number,
  database: string,
  sql: string = "",
  schema: string | null = null,
): Tab {
  const id = nextId++;
  return {
    id,
    kind: "query",
    serverId,
    database,
    schema,
    sql,
    initialSql: sql,
    results: [],
    activeResultId: null,
    lastError: null,
    editorWarning: null,
    runningQuery: false,
    loadingMore: false,
    erd: null,
  };
}

/** Create a new ERD tab for `schema`, seeded with `seed` tables (or "all").
 *  Carries no editor buffer or result; the diagram owns its own state. */
export function makeErdTab(
  serverId: number,
  database: string,
  schema: string,
  seed: string[] | "all",
  includeNeighbors: boolean,
): Tab {
  const id = nextId++;
  return {
    id,
    kind: "erd",
    serverId,
    database,
    schema,
    sql: "",
    initialSql: "",
    results: [],
    activeResultId: null,
    lastError: null,
    editorWarning: null,
    runningQuery: false,
    loadingMore: false,
    erd: { schema, seed, includeNeighbors },
  };
}

/** Test-only: reset the id counter so test ids start at 1.  Not used by app code. */
export function __resetTabIds(): void {
  nextId = 1;
  nextPaneId = 1;
}
