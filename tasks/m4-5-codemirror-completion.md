# M4.5 — CodeMirror completion source + auto-quote + case-insensitive matching

## Goal

**Before (post-M4.4):** The Editor (`src/lib/Editor.svelte`) is a CodeMirror 6 SQL editor with PostgreSQL syntax highlighting, undo/redo, Ctrl+Enter execution, and statement-boundary parsing — but **no autocomplete**. The backend can analyze the cursor (`api.analyzeCompletion`) and serve a full schema payload (`api.getSchemaPayload` via `schemaStore.getSchemaPayload`); the front-end has all the inputs it needs.

**After:** When the user types a letter, presses `Ctrl-Space`, or types `.` after an identifier, CodeMirror's autocomplete panel pops up with the right entries:

- **Schemas** in `FROM`/`JOIN` position and as the first segment of qualified names.
- **Tables / views / matviews / partitioned tables** after a schema qualifier, and unqualified when the schema appears in `search_path`.
- **Columns** after a table-or-alias qualifier, and unqualified when at least one table is in scope (the `FROM` clause's tables).
- **Keywords** always.

Matching is **case-insensitive**: typing `sel` suggests `SELECT`, typing `USE` suggests `users`. Suggested identifiers that **need quoting** (mixed case, non-alphanumeric chars, starts with digit, reserved word) insert with double quotes; everything else inserts bare. Schema-qualified suggestions auto-quote each segment independently.

The data flow is what M4.4 set up: the source calls `api.analyzeCompletion(sql, cursor)` for cursor context (cheap; no DB I/O), and `schemaStore.getSchemaPayload(serverId, database)` for the data (cached per session). No CodeMirror keystroke ever opens a Postgres connection; the only data-bearing fetch is the first one per `(server, database)`.

## Current state

### `src/lib/Editor.svelte` (post-M3.5)

The full file is reproduced in `tasks/m3-5-codemirror-editor.md`. Key facts:

- `EditorView` mounted in `onMount`, destroyed in cleanup.
- Extensions composed manually: `lineNumbers`, `highlightActiveLine`, `history`, `bracketMatching`, `indentOnInput`, `syntaxHighlighting(defaultHighlightStyle)`, `sql({ dialect: PostgreSQL })`, the standard keymaps, our `makeRunKeymap()`, an `updateListener` for `onChange`, and a `theme` override.
- Two props (`initial`, `onChange`, `onRun`) and two exported methods (`setDoc`, `focus`).
- **No `@codemirror/autocomplete` dep yet.** This task adds it.

### `src/routes/+page.svelte` (post-M3.5)

```ts
let selectedDb = $state<{ serverId: number; database: string } | null>(null);
```

Set by clicking a tree node. M4.5 passes a getter that reads it into the Editor.

### `src/lib/schemaStore.ts` (post-M4.4)

```ts
export function getSchemaPayload(serverId: number, database: string): Promise<SchemaPayload>;
export function clearSchemaPayload(serverId: number, database: string): void;
export function clearServerSchemaPayloads(serverId: number): void;
```

The completion source calls `getSchemaPayload` exactly once per trigger (per `(serverId, database)` per session). The promise resolves to `SchemaPayload { v, schemas, search_path }` — everything needed.

### `src/lib/tauri.ts` (post-M4.3)

```ts
api.analyzeCompletion(sql: string, cursor: number) => Promise<CompletionContext>;
```

Returns `{ kind, qualifier, prefix, from_offset, scope_tables }`.

## Design choices baked into this spec

- **One library, `@codemirror/autocomplete`.** The official CodeMirror 6 autocomplete extension. Provides the panel UI, filter-while-typing logic, `validFor` keyed caching, and ranked sorting.
- **Single custom source, `override: [makeSource(getContext)]`.** Don't combine with `@codemirror/lang-sql`'s built-in source (which knows nothing about Quill's schema). The keyword completions are folded into our source so suggestions stay sorted and de-duplicated.
- **Source returns `null` if no DB is selected.** No completion panel shows up until the user has clicked a database in the tree. Better than showing keywords only with no context.
- **`validFor: /^[A-Za-z0-9_]*$/`.** While the user types word characters, CodeMirror filters the cached completion list locally. Backspaces inside the prefix work the same way. Typing a `.`, space, `,`, etc. invalidates and refires the source.
- **One `analyzeCompletion` call per trigger.** The Tauri IPC is sub-millisecond; calling on every trigger is fine. `validFor` keeps adjacent keystrokes from triggering.
- **Auto-quote rules baked in TS, not on the backend.** Quoting is a *rendering* decision: the same identifier might be quoted in one position and bare in another (rare, but theoretically possible — e.g. inside a function-call argument vs. as a column ref). Keeping the logic in the source makes context-aware tweaks easy.
- **Case folding mirrors Postgres semantics.** Unquoted identifiers fold to lowercase server-side; matching folds the *prefix* to lowercase and compares against the *lowercased* identifier. Display the canonical form from the catalog (already-lowercased unless the user originally quoted the name on `CREATE TABLE`).
- **Don't depend on `@codemirror/lang-sql`'s schema-aware completion.** Its dialect doesn't model search_path, qualifier disambiguation, or our caching layer.
- **Reserved-word list is small and pragmatic.** A static hard-coded list of ~70 Postgres reserved words is enough; the auto-quoter conservatively quotes anything matching the list. Missing words occasionally fail to quote — Postgres returns a syntax error and the user adds quotes themselves. Acceptable v1 trade-off.
- **Keyword completions** are a static list (~50 common SQL keywords). Always offered; lowest boost so column/table matches outrank them when the prefix could match either.
- **No `info` panel.** v1 keeps suggestions to a one-line `label` + `detail`. Hover info (`info: "<docstring>"`) can land in v1.1.

## Deliverables

### 1. `package.json` — new dep

```bash
pnpm add @codemirror/autocomplete@^6
```

The resulting block:

```json
"@codemirror/autocomplete": "^6",
```

`pnpm-lock.yaml` updates. No build approvals expected (`@codemirror/*` packages are pure JS).

### 2. `src/lib/completion.ts` — new module (~280 lines)

The completion source plus its helpers. Sections:

```ts
//! CodeMirror 6 autocomplete source for Quill's SQL editor.
//!
//! Data flow per trigger:
//!   1. CodeMirror invokes `source(ctx)`.
//!   2. We read the current `(serverId, database)` via the `getContext`
//!      callback supplied at construction time.  Null → return null (no
//!      panel).
//!   3. Call `api.analyzeCompletion(sql, cursor)` for cursor context.
//!   4. Await `schemaStore.getSchemaPayload(serverId, database)` for
//!      schema data (cached after first call).
//!   5. Branch on `context.kind`; build a flat `Completion[]`.
//!   6. Apply case-insensitive prefix filter, auto-quote where needed,
//!      return `{ from, options, validFor }`.

import type { CompletionContext, CompletionResult, Completion } from "@codemirror/autocomplete";
import { api, type SchemaPayload, type CompletionKind, type ScopeTable } from "./tauri";
import { getSchemaPayload } from "./schemaStore";

export type EditorContext = { serverId: number; database: string } | null;

/** Build a CodeMirror completion source bound to a context getter. */
export function makeCompletionSource(getContext: () => EditorContext) {
  return async function source(
    ctx: CompletionContext,
  ): Promise<CompletionResult | null> {
    const editorCtx = getContext();
    if (editorCtx === null) return null;

    const doc = ctx.state.doc.toString();
    const cursor = ctx.pos;

    // Bail early if the user is typing nothing and didn't explicitly trigger.
    // CodeMirror's `explicit` flag is true when the user pressed Ctrl-Space.
    const triggeringChar = doc[cursor - 1];
    if (!ctx.explicit && !isAutotriggerChar(triggeringChar)) return null;

    let analysis;
    let payload: SchemaPayload;
    try {
      [analysis, payload] = await Promise.all([
        api.analyzeCompletion(doc, cursor),
        getSchemaPayload(editorCtx.serverId, editorCtx.database),
      ]);
    } catch {
      // Either call failed — silently no-op rather than poisoning the editor.
      return null;
    }

    if (analysis.kind === "none") return null;

    const options = buildOptions(analysis, payload);
    if (options.length === 0) return null;

    return {
      from: analysis.from_offset,
      options,
      validFor: /^[A-Za-z0-9_]*$/,
    };
  };
}

/** A character that should auto-trigger the completion panel when typed. */
function isAutotriggerChar(c: string | undefined): boolean {
  if (!c) return false;
  // Letters, digits, underscore, dot — same set CodeMirror considers
  // identifier-continuing for our SQL dialect.  Quote chars deliberately
  // excluded — we don't want to pop suggestions inside a string literal
  // even when the analyzer fails to detect it.
  return /[A-Za-z0-9_.]/.test(c);
}

// ── Option builders ────────────────────────────────────────────────────

function buildOptions(
  analysis: {
    kind: CompletionKind;
    qualifier: string | null;
    prefix: string;
    scope_tables: ScopeTable[];
  },
  payload: SchemaPayload,
): Completion[] {
  const prefix = analysis.prefix.toLowerCase();

  switch (analysis.kind) {
    case "from_item":
      return [
        ...schemaCompletions(payload, prefix),
        ...tablesInSearchPath(payload, prefix),
        ...keywordCompletions(prefix),
      ];

    case "qualified_relation": {
      const schemaName = analysis.qualifier;
      if (!schemaName) return [];
      return relationsInSchema(payload, schemaName, prefix);
    }

    case "qualified_column": {
      const qualifier = analysis.qualifier;
      if (!qualifier) return [];
      return columnsForQualifier(
        payload,
        analysis.scope_tables,
        qualifier,
        prefix,
      );
    }

    case "unqualified":
      return [
        ...unqualifiedColumns(payload, analysis.scope_tables, prefix),
        ...keywordCompletions(prefix),
      ];

    default:
      return [];
  }
}

function schemaCompletions(payload: SchemaPayload, prefix: string): Completion[] {
  return payload.schemas
    .filter((s) => s.name.toLowerCase().startsWith(prefix))
    .map((s) => ({
      label: s.name,
      apply: autoQuote(s.name),
      type: "namespace",
      detail: "schema",
      boost: 10,
    }));
}

function tablesInSearchPath(payload: SchemaPayload, prefix: string): Completion[] {
  const out: Completion[] = [];
  for (const schemaName of payload.search_path) {
    const schema = payload.schemas.find((s) => s.name === schemaName);
    if (!schema) continue;
    for (const rel of schema.relations) {
      if (!rel.name.toLowerCase().startsWith(prefix)) continue;
      out.push({
        label: rel.name,
        apply: autoQuote(rel.name),
        type: relationType(rel.kind),
        detail: `${schemaName}.${rel.name} (${rel.kind})`,
        boost: 5,
      });
    }
  }
  return out;
}

function relationsInSchema(
  payload: SchemaPayload,
  schemaName: string,
  prefix: string,
): Completion[] {
  const schema = payload.schemas.find(
    (s) => s.name.toLowerCase() === schemaName.toLowerCase(),
  );
  if (!schema) return [];
  return schema.relations
    .filter((r) => r.name.toLowerCase().startsWith(prefix))
    .map((r) => ({
      label: r.name,
      apply: autoQuote(r.name),
      type: relationType(r.kind),
      detail: r.kind,
    }));
}

function columnsForQualifier(
  payload: SchemaPayload,
  scope: ScopeTable[],
  qualifier: string,
  prefix: string,
): Completion[] {
  // Find the scope_table whose alias OR name matches the qualifier.
  const q = qualifier.toLowerCase();
  const match = scope.find(
    (t) => (t.alias ?? t.name).toLowerCase() === q,
  );
  if (!match) return [];

  // Resolve the actual relation via schema (if any) then name.
  const schemaCandidates = match.schema
    ? payload.schemas.filter((s) => s.name === match.schema)
    : payload.schemas.filter((s) => payload.search_path.includes(s.name));

  for (const schema of schemaCandidates) {
    const rel = schema.relations.find(
      (r) => r.name.toLowerCase() === match.name.toLowerCase(),
    );
    if (!rel) continue;
    return rel.columns
      .filter((c) => c.name.toLowerCase().startsWith(prefix))
      .map((c) => ({
        label: c.name,
        apply: autoQuote(c.name),
        type: "property",
        detail: c.type_name + (c.not_null ? " NOT NULL" : ""),
      }));
  }
  return [];
}

function unqualifiedColumns(
  payload: SchemaPayload,
  scope: ScopeTable[],
  prefix: string,
): Completion[] {
  const out: Completion[] = [];
  // Collect columns of every scope table; tag by source table for `detail`.
  for (const t of scope) {
    const schemaCandidates = t.schema
      ? payload.schemas.filter((s) => s.name === t.schema)
      : payload.schemas.filter((s) => payload.search_path.includes(s.name));
    for (const schema of schemaCandidates) {
      const rel = schema.relations.find(
        (r) => r.name.toLowerCase() === t.name.toLowerCase(),
      );
      if (!rel) continue;
      for (const c of rel.columns) {
        if (!c.name.toLowerCase().startsWith(prefix)) continue;
        out.push({
          label: c.name,
          apply: autoQuote(c.name),
          type: "property",
          detail: `${t.alias ?? t.name}.${c.name} : ${c.type_name}`,
          boost: 8,
        });
      }
      break; // first match wins for unqualified resolution
    }
  }
  return out;
}

function relationType(kind: string): Completion["type"] {
  switch (kind) {
    case "table": return "class";
    case "view": return "class";
    case "matview": return "class";
    case "partitioned_table": return "class";
    default: return "variable";
  }
}

// ── Keyword completions ────────────────────────────────────────────────

const KEYWORDS: readonly string[] = [
  "SELECT", "FROM", "WHERE", "GROUP BY", "ORDER BY", "HAVING", "LIMIT",
  "OFFSET", "JOIN", "INNER JOIN", "LEFT JOIN", "RIGHT JOIN", "FULL JOIN",
  "CROSS JOIN", "ON", "USING", "AND", "OR", "NOT", "NULL", "TRUE", "FALSE",
  "IS", "IS NULL", "IS NOT NULL", "IN", "EXISTS", "BETWEEN", "LIKE", "ILIKE",
  "AS", "DISTINCT", "ALL", "UNION", "INTERSECT", "EXCEPT", "WITH", "CASE",
  "WHEN", "THEN", "ELSE", "END", "RETURNING", "INSERT INTO", "VALUES",
  "UPDATE", "SET", "DELETE FROM", "CREATE TABLE", "DROP TABLE", "ALTER TABLE",
  "CREATE INDEX", "CREATE VIEW", "BEGIN", "COMMIT", "ROLLBACK", "SAVEPOINT",
];

function keywordCompletions(prefix: string): Completion[] {
  if (prefix.length === 0) {
    // Don't flood the panel with all 50 keywords when the cursor is at a
    // whitespace position with no prefix typed.  Wait for at least one
    // letter before offering keyword completions.
    return [];
  }
  return KEYWORDS
    .filter((kw) => kw.toLowerCase().startsWith(prefix))
    .map((kw) => ({
      label: kw,
      type: "keyword",
      boost: -10,
    }));
}

// ── Auto-quote ─────────────────────────────────────────────────────────

/** Postgres reserved-word set used by the auto-quoter.  Far from
 *  exhaustive — picks the common ones that show up as table/column names
 *  in real schemas.  Items here force quoting even if the rest of the
 *  identifier looks bare. */
const RESERVED: ReadonlySet<string> = new Set([
  "all", "analyse", "analyze", "and", "any", "array", "as", "asc",
  "asymmetric", "both", "case", "cast", "check", "collate", "column",
  "constraint", "create", "current_catalog", "current_date", "current_role",
  "current_time", "current_timestamp", "current_user", "default",
  "deferrable", "desc", "distinct", "do", "else", "end", "except", "false",
  "fetch", "for", "foreign", "from", "grant", "group", "having", "in",
  "initially", "intersect", "into", "lateral", "leading", "limit",
  "localtime", "localtimestamp", "not", "null", "offset", "on", "only",
  "or", "order", "placing", "primary", "references", "returning", "select",
  "session_user", "some", "symmetric", "table", "then", "to", "trailing",
  "true", "union", "unique", "user", "using", "variadic", "when", "where",
  "window", "with",
]);

/** Decide whether `id` needs `"…"` quoting in SQL output.  Pure function. */
export function needsQuoting(id: string): boolean {
  if (id.length === 0) return false;
  // Starts with digit → must quote.
  if (/^[0-9]/.test(id)) return true;
  // Contains anything outside [A-Za-z0-9_] → must quote.
  if (/[^A-Za-z0-9_]/.test(id)) return true;
  // Has any uppercase → must quote (unquoted folds to lowercase server-side).
  if (id !== id.toLowerCase()) return true;
  // Reserved word → must quote.
  if (RESERVED.has(id.toLowerCase())) return true;
  return false;
}

/** Return the SQL form of an identifier — bare or `"…"` with doubled
 *  inner quotes.  Pure. */
export function autoQuote(id: string): string {
  if (!needsQuoting(id)) return id;
  return `"${id.replaceAll('"', '""')}"`;
}
```

### 3. `src/lib/Editor.svelte` — wire the autocomplete extension

The editor accepts a new prop `getContext: () => EditorContext` (default `() => null`). Extensions list gains `autocompletion({ override: [makeCompletionSource(getContext)] })`:

```svelte
<script lang="ts">
  // existing imports...
  import { autocompletion } from "@codemirror/autocomplete";
  import { makeCompletionSource, type EditorContext } from "./completion";

  let {
    initial = "SELECT 1",
    onChange,
    onRun,
    getContext = () => null as EditorContext,
  }: {
    initial?: string;
    onChange: (doc: string) => void;
    onRun: (payload: {
      text: string;
      isSelection: boolean;
      multiStatement: boolean;
    }) => void;
    getContext?: () => EditorContext;
  } = $props();

  // ...

  onMount(() => {
    if (!host) return;
    const state = EditorState.create({
      doc: initial,
      extensions: [
        lineNumbers(),
        highlightActiveLine(),
        history(),
        bracketMatching(),
        indentOnInput(),
        syntaxHighlighting(defaultHighlightStyle),
        sql({ dialect: PostgreSQL }),
        autocompletion({
          override: [makeCompletionSource(getContext)],
          closeOnBlur: true,
          activateOnTyping: true,
          // Default `maxRenderedOptions: 100` is fine for v1.
        }),
        keymap.of([
          ...defaultKeymap,
          ...historyKeymap,
          ...searchKeymap,
          indentWithTab,
        ]),
        makeRunKeymap(),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) onChange(u.state.doc.toString());
        }),
        EditorView.theme({ /* unchanged */ }),
      ],
    });
    view = new EditorView({ state, parent: host });
    return () => {
      view?.destroy();
      view = null;
    };
  });
</script>
```

Note: `getContext` is a function (not a value) so it stays current as the parent's `selectedDb` reactivity changes. CodeMirror calls it on every trigger; no need to recreate the editor when the database changes.

### 4. `src/routes/+page.svelte` — pass the context getter

```svelte
<Editor
  bind:this={editor}
  initial={sql}
  onChange={(doc) => { sql = doc; }}
  onRun={(payload) => runFromEditor(payload)}
  getContext={() => selectedDb}
/>
```

`selectedDb` is already the right shape: `{ serverId: number; database: string } | null`. No other edits in this file.

## Implementation order

1. `pnpm add @codemirror/autocomplete@^6` — `pnpm-lock.yaml` updates.
2. Write `src/lib/completion.ts` end-to-end. Don't wire into the Editor yet. Run `pnpm check` — should pass clean (no callers).
3. Edit `src/lib/Editor.svelte` to accept `getContext` and add the `autocompletion` extension. `pnpm check`.
4. Edit `src/routes/+page.svelte` to pass `getContext={() => selectedDb}`. `pnpm check`.
5. **`./run.sh` smoke test** (full sequence below).
6. Optionally add a `completion.test.ts` once Vitest exists. v1 ships without.

## Known gotchas

- **CodeMirror's `CompletionContext` ≠ Quill's `CompletionContext`.** Same name, different shape. Inside `completion.ts`, the CodeMirror type is imported from `@codemirror/autocomplete` as `CompletionContext`; the Quill backend type is referenced inline via `CompletionKind` and `ScopeTable` re-exports from `tauri.ts`. **Don't import Quill's `CompletionContext` here** — rename inline or destructure into the analysis call so the type collision never surfaces.
- **`activateOnTyping: true` triggers on every word char.** Coupled with `validFor` it doesn't fire Tauri on every key; the source only re-runs when the regex stops matching. Good defaults — don't disable.
- **`closeOnBlur: true`** dismisses the panel when the user clicks elsewhere. Important for the Run button case: clicking Run while the panel is open should run the query and dismiss the panel, not run-with-completion-still-showing.
- **`source` is async but must not throw.** A thrown error propagates to CodeMirror and silently breaks completion until the editor remounts. Wrap the `Promise.all` in `try/catch` and return `null` on error. The completion source is best-effort.
- **`validFor` regex must not include `.`.** Typing `users.` should *retrigger* the source so the kind changes from `from_item` to `qualified_relation`. The regex `^[A-Za-z0-9_]*$` excludes `.` precisely for this reason.
- **First fetch latency.** The first time the user triggers completion against a fresh database, the source awaits both `analyzeCompletion` (fast) *and* `getSchemaPayload` (which may run a full introspection — hundreds of ms on big schemas). CodeMirror shows nothing until the promise resolves; the panel pops up when it does. Subsequent triggers are local (`validFor` cache or store cache). This is the correct behaviour; do not preload.
- **`autoQuote` is called on `apply`, not `label`.** The user sees `mixedCase` in the panel and gets `"mixedCase"` inserted. This matches how every database client behaves and is the least surprising.
- **`autoQuote("public.users")` would incorrectly produce `"public.users"`** (a single quoted name with a literal dot) — but we *never call `autoQuote` on a dotted form*. Schema-qualified suggestions in `qualifiedRelation` already split the qualifier from the relation; the source returns only the relation name as `apply`, which inserts after the existing `schema.`. Don't pass dotted strings through `autoQuote`.
- **Case-insensitive matching is one direction only.** We lowercase the *prefix* and compare against the *lowercased* identifier. The display retains the catalog's canonical case (typically lowercase; preserved if the user originally quoted). Don't lowercase the `label`.
- **Reserved-word list is incomplete on purpose.** It includes the ~70 most common Postgres reserved keywords (per the Postgres SQL grammar appendix). Edge cases (e.g. `analyse` vs `analyze`) appear because Postgres reserves both. If a user files a "missing quote" bug we add the keyword; we don't pull in the full ~470-keyword catalog (`pg_get_keywords()` would be canonical but we'd ship it as data, not code).
- **`pgKeywords` from `@codemirror/lang-sql`** is conceptually appealing but the keyword list and the reserved-word list are different things — keywords get autocomplete suggestions, reserved words trigger quoting. Don't conflate.
- **`Completion.type` semantics.** CodeMirror's themed icons recognise: `keyword`, `class` (relation), `property` (column), `namespace` (schema), `variable`, `function`, `type`, `text`. The CSS for icons is themable. Use the standard names; M6 may polish them.
- **`boost` ordering.** Numbers higher = ranked higher. `keyword: -10` ensures column/table completions outrank keywords; `column: 8`, `schema: 10` keep the schema-first / column-second / keyword-last default ordering.
- **`@codemirror/autocomplete` peer deps.** It depends on `@codemirror/state` and `@codemirror/view`, both already in deps. `pnpm install` should succeed without prompts.
- **Don't add `@codemirror/lang-sql`'s `SQLConfig.schema`.** That config takes a static map; we generate completions dynamically. Mixing the two leads to duplicate options in the panel.
- **`@codemirror/autocomplete` does NOT need `EditorView.lineWrapping`.** Unrelated.
- **`getContext` is a function, not a `$state` proxy.** The completion source captures the closure once at construction. Re-creating the editor when `selectedDb` changes is *not* the design — the closure already reads the latest value because the parent's `selectedDb` is a runed `$state` whose getter returns the current snapshot. Verify with the smoke test: change databases and re-trigger completion; the suggestions reflect the new DB.
- **Scope-table resolution and qualified columns.** `scope_tables[i].schema` may be `None` (user wrote `FROM users` not `FROM public.users`). The source then walks `search_path` schemas in order and uses the first one where a relation matches the name. This is exactly Postgres' resolution rule. *Don't* fall back to "any schema" — that would suggest the same column from multiple unrelated tables.
- **Empty prefix + `unqualified` kind:** the source returns columns from all scope tables (potentially dozens). That's the expected v1 behaviour — `Ctrl-Space` after `SELECT ` shows every available column. CodeMirror's panel is scrollable. If a user complains the panel is overwhelming, M6 polish can prefer recently-used columns.
- **No multi-statement scope.** If the cursor sits in statement #2, the M4.3 analyzer only inspects statement #2's FROM clause. So `SELECT * FROM users; SELECT u.<TAB>` returns no columns — `u` isn't in scope of statement #2. Document this; the user adds a FROM to statement #2.

## Tests

No new automated tests in this task (Vitest still not set up). Coverage is end-to-end via the smoke test.

### Manual smoke test

```bash
./run.sh
```

For each test, the precondition is: connect to a Postgres server, expand a database, click the database in the tree so `selectedDb` is set.

1. **Schemas in FROM position.** Type `SELECT * FROM ` and pause. Panel pops up listing schemas (e.g. `public`, `quill_m41_fixture`).
2. **Tables after a schema qualifier.** Type `SELECT * FROM public.` — panel switches to `users`, `orders`, etc. (relations in `public`).
3. **Tables in search_path unqualified.** Type `SELECT * FROM u` — panel suggests `users` (because `public` is on the `search_path`).
4. **Columns after a table qualifier.** Type `SELECT u. FROM users u` — panel suggests `id`, `email`, `signup_at` (columns of `public.users`).
5. **Unqualified columns with scope.** Type `SELECT em FROM users` — `email` is suggested with detail `users.email : text`.
6. **Multi-table scope with aliases.** Type `SELECT  FROM users u JOIN orders o ON u.id = o.user_id` (cursor right after `SELECT `). Press `Ctrl-Space`. Panel lists columns from both `users` and `orders`, each with the alias prefix in detail.
7. **Keywords appear, lower-ranked.** Type `SE` — `SELECT` appears. With columns starting with `se` in scope, columns rank higher.
8. **Case-insensitive matching.** Type `useR` (mixed case) — `users` is suggested. The label remains lowercase; insertion is `users`.
9. **Auto-quote for mixed case.** In a database that has a table `"MixedCase"`, type `Mi` — suggestion shows `MixedCase`; pressing Enter inserts `"MixedCase"`.
10. **Auto-quote for reserved word.** If you have a column named `select` (yes, legal in Postgres if quoted at creation), typing `sel` in column position suggests it; insertion is `"select"`.
11. **No panel inside a string.** Type `SELECT 'hello ` and pause — no panel. Stringliteral suppression is correct.
12. **No panel inside a comment.** Type `-- select ` and pause — no panel.
13. **Refresh invalidates.** Right-click a database → Refresh. Trigger completion again; suggestions reflect any DDL changes made since the last load.
14. **Disconnect clears.** Right-click server → Disconnect → reconnect. First completion trigger refetches the payload.
15. **No DB selected → no panel.** Disconnect everything; type into the editor. No completion panel.

## Acceptance criteria

- [ ] `pnpm install` succeeds and `pnpm-lock.yaml` reflects the new `@codemirror/autocomplete` entry.
- [ ] `pnpm check` succeeds clean.
- [ ] `git status -- src/lib/` shows `completion.ts` as a new file.
- [ ] `( cd src-tauri && cargo build )` succeeds (no backend changes expected from this task; sanity check).
- [ ] `./test.sh` succeeds (no new tests expected).
- [ ] `grep -F '@codemirror/autocomplete' package.json` returns one match.
- [ ] `grep -n 'autocompletion(' src/lib/Editor.svelte` shows the extension registered.
- [ ] `grep -n 'getContext' src/routes/+page.svelte` shows the prop passed.
- [ ] `grep -n 'autoQuote\|needsQuoting' src/lib/completion.ts` shows both helpers exported.
- [ ] Smoke step 1 — schemas pop up after `FROM `.
- [ ] Smoke step 2 — relations pop up after `public.`.
- [ ] Smoke step 4 — columns pop up after `<alias>.`.
- [ ] Smoke step 8 — `useR` suggests `users`.
- [ ] Smoke step 9 — mixed-case table inserts with quotes.
- [ ] Smoke step 11 — no panel inside `'…'` string.
- [ ] Smoke step 13 — Refresh causes a refetch (confirm with devtools logging the second call to `getSchemaPayload`).
- [ ] Smoke step 15 — completion is silent when no DB is selected.
- [ ] `grep -F 'EditorView.lineWrapping' src/lib/Editor.svelte` returns zero matches (didn't accidentally enable wrapping).
- [ ] `grep -F '@codemirror/lang-sql' src/lib/completion.ts` returns zero matches — completion source doesn't reach back into the lang pack.

## Out of scope

- **Function argument completion** (`fn(<TAB>`) — v1.1. M4.1 deliberately didn't extend `FunctionInfo` with argument metadata.
- **CTE alias resolution** (`WITH foo AS (...) SELECT foo.<TAB>`) — v1.1 (M4 brief explicitly defers).
- **Subquery alias resolution** (`SELECT * FROM (SELECT id FROM users) u WHERE u.<TAB>`) — v1.1.
- **Hover info / docstrings** (`Completion.info`) — v1.1 polish.
- **Snippet templates** (`SELECT <fields> FROM <table> WHERE <cond>`) — v1.1.
- **`@codemirror/theme-one-dark` integration** — **M6**.
- **Recently-used ranking** — M6 polish.
- **`schema_path`-aware boost ordering** (preferring schemas earlier in `search_path`) — works correctly today via first-match-wins resolution; explicit boosts would be M6.
- **Vitest setup** — separate task. M4.5 ships with manual smoke only.
- **UTF-16 vs UTF-8 cursor handling** for non-ASCII identifiers — M4.3 documents the v1 punt; M4.5 inherits it. ASCII SQL works perfectly.
- **Schema-qualified function completion after `.`** (`schema.fn(`) — analyzer returns `qualified_relation` for now since v1 doesn't model function-namespace lookup. Acceptable v1 behaviour.
