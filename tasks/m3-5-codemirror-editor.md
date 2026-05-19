# M3.5 — CodeMirror 6 SQL editor

## Goal

**Before (post-M3.4):** The right pane uses a plain `<textarea>` bound to `sql: $state<string>`. There is no syntax highlighting, no undo/redo, no Ctrl+Enter shortcut. The Run button reads the whole textarea value and submits it to `api.runQuery`. M3.4's backend already supports `runQuery` returning `RunResult` + `result_id`, but the editor side is unchanged from M1.6 / M2.4.

**After:** The textarea is replaced by a CodeMirror 6 component (`src/lib/Editor.svelte`) configured with the PostgreSQL SQL dialect, default theme, monospace font, line numbers, and the standard keymap (undo / redo / find / comment toggle). A new `Cmd/Ctrl+Enter` keybinding fires a `run` callback with the **current statement or selection** — top-level `;` splits the buffer, and the statement under the cursor is the one that runs. If a selection exists, the selection runs verbatim. If the buffer has multiple statements and there's no selection, only the statement at the cursor runs (M3 explicitly defers multi-statement scripts).

An inline error region renders below the editor, populated by `+page.svelte` when `runQuery` rejects. Result rendering still uses the existing `<pre>` (M3.6 replaces that with a grid).

This task is **frontend only**. The backend introduced in M3.4 is untouched.

## Current state

### `src/routes/+page.svelte` (post-M3.4)

The right pane contains:
- `<textarea bind:value={sql} class="sql-input" rows={8} placeholder="SELECT 1" />`
- A Run button calling `run()` which invokes `api.runQuery(selectedDb.serverId, selectedDb.database, sql)`.
- A `<pre>{renderResult(result)}</pre>` block below.

The `result` state is now typed as `RunResult | { error: CommandError } | null`.

### `package.json`

```json
"dependencies": {
  "@tauri-apps/api": "^2",
  "@tauri-apps/plugin-opener": "^2"
}
```

No CodeMirror yet. M2.4's explicit "no new pnpm deps" is now lifted: M3.5 adds the CodeMirror set.

### `src/lib/`

- `tauri.ts` — Tauri bridge.
- `tree.ts`, `Tree.svelte` — left-pane tree.

This task adds `Editor.svelte`, `statement.ts` (the boundary heuristic), and a small `editorTheme.ts` if a theme override is needed (probably not in v1).

## Design choices baked into this spec

- **CodeMirror 6, not 5.** ESM-native, modular, idiomatic in modern Svelte. v6 is the only choice.
- **Minimal dep set.** `@codemirror/state`, `@codemirror/view`, `@codemirror/lang-sql`, `@codemirror/commands`. No `@codemirror/basic-setup` — it pulls in everything; we want to compose extensions explicitly to keep the bundle tight. No theme package in v1 — default CodeMirror theme is fine; M6 owns dark mode.
- **Mount inside `$effect`.** SvelteKit's `adapter-static` server-renders empty HTML and hydrates in the browser, but Tauri itself never SSRs. Still, CodeMirror only works in the browser, so the mount goes inside `$effect(() => { ... })` whose body returns a cleanup function. `onMount` would work too — `$effect` is the Svelte 5 idiom.
- **EditorView is a `$state<EditorView | null>(null)`.** The component exposes imperative methods (`focus()`, `getDoc()`, `setDoc(s)`) via callbacks/refs since two-way binding doesn't translate well to CodeMirror's transactional model. The parent reads the doc via a callback on each `onChange`.
- **Statement boundary detection lives in `src/lib/statement.ts`** as a pure function. Easier to test, easier to reuse in M4 (autocomplete also walks the buffer).
- **The boundary heuristic is intentionally simple.** Track three states: in single-quote string, in identifier-quote string, in line comment, in block comment, in dollar-quoted string. Top-level `;` outside all five is a separator. Anything trickier than that (e.g. `$$ ... $$` with nested `$tag$ ... $tag$` ) is out of scope for v1 — `sqlparser-rs` enters in M4 and the heuristic can be replaced or augmented then.
- **Ctrl/Cmd+Enter fires `run` with the statement (or selection).** The keymap binds a function that calls the prop callback. The selection-vs-statement decision happens in JS, not Postgres.
- **Inline errors render below the editor**, not in a modal. Same place where Load-more / status will eventually live (M3.6).
- **The component is not a typed bridge.** It exposes ordinary props/callbacks. Parent owns the document state (`sql: $state<string>`) — the editor is a controlled-ish component.

## Deliverables

### 1. `package.json` — new deps

Run via `pnpm`:

```bash
pnpm add @codemirror/state@^6 @codemirror/view@^6 @codemirror/lang-sql@^6 @codemirror/commands@^6 @codemirror/language@^6 @codemirror/search@^6
```

(`@codemirror/language` and `@codemirror/search` are runtime deps of `lang-sql` and `commands` — pnpm hoists them but listing them explicitly keeps semver pins visible.)

The resulting `dependencies` block adds:

```json
"@codemirror/state": "^6",
"@codemirror/view": "^6",
"@codemirror/lang-sql": "^6",
"@codemirror/commands": "^6",
"@codemirror/language": "^6",
"@codemirror/search": "^6"
```

Run `pnpm approve-builds` for any package that prompts. No native scripts are expected.

### 2. `src/lib/statement.ts` — boundary heuristic + extraction

```ts
//! Cheap SQL statement-boundary heuristic for v1.
//!
//! Tracks five lexer states (single-quote string, double-quote ident,
//! line comment, block comment, dollar-quoted string) and treats any
//! top-level `;` as a separator.  Anything more elaborate (nested
//! dollar-quotes, schema-qualified backslash commands, psql meta-syntax)
//! is out of scope; M4 replaces this with `sqlparser-rs` calls.

export type StatementSpan = {
  /** Character offset (inclusive) where the statement starts. */
  from: number;
  /** Character offset (exclusive) where the statement ends. */
  to: number;
  /** The statement text, trimmed of leading/trailing whitespace. */
  text: string;
};

/** Split a SQL buffer into statement spans, separated by top-level `;`. */
export function splitStatements(sql: string): StatementSpan[] {
  const spans: StatementSpan[] = [];
  let stmtStart = 0;
  let i = 0;
  let inSingle = false;
  let inIdent = false; // double-quoted identifier
  let inLine = false;
  let inBlock = false;
  let dollarTag: string | null = null; // null when not in $tag$ ... $tag$

  while (i < sql.length) {
    const c = sql[i];
    const c2 = sql[i + 1] ?? "";

    if (inLine) {
      if (c === "\n") inLine = false;
      i++;
      continue;
    }
    if (inBlock) {
      if (c === "*" && c2 === "/") {
        inBlock = false;
        i += 2;
        continue;
      }
      i++;
      continue;
    }
    if (dollarTag !== null) {
      // Look for closing $tag$
      if (
        c === "$" &&
        sql.slice(i, i + dollarTag.length) === dollarTag
      ) {
        i += dollarTag.length;
        dollarTag = null;
        continue;
      }
      i++;
      continue;
    }
    if (inSingle) {
      if (c === "'" && c2 === "'") {
        i += 2; // escaped quote
        continue;
      }
      if (c === "'") inSingle = false;
      i++;
      continue;
    }
    if (inIdent) {
      if (c === '"' && c2 === '"') {
        i += 2; // escaped quote
        continue;
      }
      if (c === '"') inIdent = false;
      i++;
      continue;
    }

    // Top-level lexing
    if (c === "-" && c2 === "-") {
      inLine = true;
      i += 2;
      continue;
    }
    if (c === "/" && c2 === "*") {
      inBlock = true;
      i += 2;
      continue;
    }
    if (c === "'") {
      inSingle = true;
      i++;
      continue;
    }
    if (c === '"') {
      inIdent = true;
      i++;
      continue;
    }
    if (c === "$") {
      // Try to read $tag$ where tag is [A-Za-z_][A-Za-z0-9_]* (possibly empty)
      let j = i + 1;
      while (j < sql.length && /[A-Za-z0-9_]/.test(sql[j])) j++;
      if (sql[j] === "$") {
        dollarTag = sql.slice(i, j + 1);
        i = j + 1;
        continue;
      }
      // Bare $ — treat as ordinary char.
      i++;
      continue;
    }
    if (c === ";") {
      const text = sql.slice(stmtStart, i).trim();
      if (text.length > 0) {
        spans.push({ from: stmtStart, to: i, text });
      }
      stmtStart = i + 1;
      i++;
      continue;
    }

    i++;
  }

  const tail = sql.slice(stmtStart).trim();
  if (tail.length > 0) {
    spans.push({
      from: stmtStart,
      to: sql.length,
      text: tail,
    });
  }

  return spans;
}

/** Pick the statement to run given the current buffer + cursor + selection.
 *
 *  - If `selection.from !== selection.to`, return the selected text
 *    verbatim (no boundary parsing).
 *  - Otherwise, return the span whose `[from, to]` brackets the cursor.
 *  - If the cursor sits past the last `;` and no tail exists, return null.
 */
export function statementAtCursor(
  sql: string,
  cursor: number,
  selection: { from: number; to: number },
): { text: string; isSelection: boolean; multiStatement: boolean } | null {
  if (selection.from !== selection.to) {
    return {
      text: sql.slice(selection.from, selection.to).trim(),
      isSelection: true,
      multiStatement: splitStatements(sql).length > 1,
    };
  }

  const spans = splitStatements(sql);
  if (spans.length === 0) return null;

  for (const s of spans) {
    if (cursor >= s.from && cursor <= s.to) {
      return {
        text: s.text,
        isSelection: false,
        multiStatement: spans.length > 1,
      };
    }
  }
  // Cursor past the last span — run the last statement.
  const last = spans[spans.length - 1];
  return {
    text: last.text,
    isSelection: false,
    multiStatement: spans.length > 1,
  };
}
```

### 3. `src/lib/Editor.svelte` — CodeMirror 6 wrapper

```svelte
<script lang="ts">
  //! CodeMirror 6 SQL editor.
  //!
  //! Owns the EditorView; mirrors document changes back to the parent via
  //! the `onChange` callback.  The parent retains the source of truth in
  //! a `$state<string>` and writes back into the editor when needed via
  //! the exposed `setDoc` method (e.g. M5's "open saved query" flow).

  import { onMount } from "svelte";
  import { EditorState, Compartment } from "@codemirror/state";
  import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
  import {
    defaultKeymap,
    history,
    historyKeymap,
    indentWithTab,
  } from "@codemirror/commands";
  import { sql, PostgreSQL } from "@codemirror/lang-sql";
  import { searchKeymap } from "@codemirror/search";
  import { bracketMatching, indentOnInput } from "@codemirror/language";

  import { statementAtCursor } from "./statement";

  let {
    initial = "SELECT 1",
    onChange,
    onRun,
  }: {
    initial?: string;
    onChange: (doc: string) => void;
    onRun: (payload: {
      text: string;
      isSelection: boolean;
      multiStatement: boolean;
    }) => void;
  } = $props();

  let host = $state<HTMLDivElement | null>(null);
  let view: EditorView | null = null;

  // Public-ish imperative methods. The parent gets a reference via bind:this.
  export function setDoc(next: string) {
    if (!view) return;
    if (view.state.doc.toString() === next) return;
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: next },
    });
  }
  export function focus() {
    view?.focus();
  }

  function makeRunKeymap() {
    return keymap.of([
      {
        key: "Mod-Enter",
        preventDefault: true,
        run: (v) => {
          const doc = v.state.doc.toString();
          const cursor = v.state.selection.main.head;
          const sel = v.state.selection.main;
          const picked = statementAtCursor(doc, cursor, {
            from: sel.from,
            to: sel.to,
          });
          if (picked && picked.text.length > 0) {
            onRun(picked);
          }
          return true;
        },
      },
    ]);
  }

  $effect(() => {
    if (!host) return;

    const state = EditorState.create({
      doc: initial,
      extensions: [
        lineNumbers(),
        highlightActiveLine(),
        history(),
        bracketMatching(),
        indentOnInput(),
        sql({ dialect: PostgreSQL }),
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
        EditorView.theme({
          "&": {
            height: "100%",
            fontSize: "13px",
            fontFamily:
              "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
          },
          ".cm-content": { padding: "8px 0" },
          ".cm-scroller": { fontFamily: "inherit" },
        }),
      ],
    });

    view = new EditorView({ state, parent: host });

    return () => {
      view?.destroy();
      view = null;
    };
  });
</script>

<div class="editor-host" bind:this={host}></div>

<style>
  .editor-host {
    height: 220px;
    border: 1px solid #aaa;
    border-radius: 4px;
    overflow: hidden;
    background: white;
  }
  :global(.cm-editor) {
    height: 100%;
  }
  :global(.cm-editor.cm-focused) {
    outline: 2px solid #3366cc;
    outline-offset: -2px;
  }
</style>
```

### 4. `src/routes/+page.svelte` — swap textarea for Editor

The relevant changes in the right pane (other parts of the file unchanged):

Add imports:

```ts
import Editor from "$lib/Editor.svelte";
```

Replace state for the textarea result and add an editor ref + warning:

```ts
let sql = $state("SELECT 1");
let editor = $state<Editor | undefined>(undefined);
let editorWarning = $state<string | null>(null);
```

Replace the `<textarea>` and Run button block with:

```svelte
<Editor
  bind:this={editor}
  initial={sql}
  onChange={(doc) => { sql = doc; }}
  onRun={(payload) => runFromEditor(payload)}
/>

{#if editorWarning}
  <p class="muted">{editorWarning}</p>
{/if}

<div class="run-row">
  <button class="btn" onclick={() => runFromEditor(buildPayloadFromButton())} disabled={!canRun}>
    {runningQuery ? "Running…" : "Run (Ctrl/Cmd+Enter)"}
  </button>
</div>
```

The `runFromEditor` function:

```ts
import { statementAtCursor } from "$lib/statement";

function buildPayloadFromButton() {
  // Without cursor info from the editor (button click), pretend we have
  // cursor at end and no selection.  The editor itself uses the real
  // selection on Cmd+Enter.
  return statementAtCursor(sql, sql.length, { from: sql.length, to: sql.length });
}

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
  runningQuery = true;
  result = null;
  try {
    result = await api.runQuery(selectedDb.serverId, selectedDb.database, payload.text);
  } catch (err) {
    result = { error: err as CommandError };
  } finally {
    runningQuery = false;
  }
}
```

The old `run()` function is removed. The `canRun` derived stays roughly the same:

```ts
let canRun = $derived(
  selectedDb !== null &&
    isConnected(selectedDb.serverId) &&
    sql.trim().length > 0 &&
    !runningQuery,
);
```

Add a small `.run-row` style if needed; reuse `.btn`.

The `renderResult` function and `<pre>` block stay unchanged from M3.4 — M3.6 owns the grid swap.

## Implementation order

1. `pnpm add` the six packages.
2. Create `src/lib/statement.ts`.
3. Create `src/lib/Editor.svelte`.
4. Edit `src/routes/+page.svelte` to swap the textarea + Run button for the new component + run function. Keep the result `<pre>` as-is.
5. `pnpm check` — must pass clean.
6. `./run.sh` — manual smoke test below.

## Known gotchas

- **SvelteKit `adapter-static` + CodeMirror.** Mount inside `$effect` (or `onMount`). Top-level imports of `EditorView` are fine — they're tree-shakable; only the side-effecting `new EditorView(...)` must be deferred. SvelteKit prerenders against jsdom-ish APIs that *will* break CodeMirror initialization if you instantiate at module load.
- **Svelte 5 props syntax.** `$props()` destructuring with defaults. Don't use `export let` — Svelte 5 deprecates it for runes mode.
- **CodeMirror keymap precedence.** Our `makeRunKeymap()` comes *after* `defaultKeymap` so its `Mod-Enter` wins. CodeMirror documents this — last-registered keymap binding takes priority on conflict.
- **`Mod-Enter` is the cross-platform binding.** CodeMirror maps `Mod` to `Cmd` on macOS and `Ctrl` everywhere else. Don't bind both `Ctrl-Enter` and `Cmd-Enter`.
- **`statementAtCursor` returns `null` for empty buffers.** Handle this in `runFromEditor` to show "Nothing to run".
- **Selection always wins over cursor.** Even single-character selections suppress the boundary parse — matches user intuition.
- **The boundary heuristic is not airtight.** Dollar-quoted strings with `$$` and nested differing tags work; psql `\d` commands at top level look like normal SQL and Postgres errors on them — that's fine. Don't over-engineer; M4 brings `sqlparser-rs` to the backend.
- **Bracket matching from `@codemirror/language`.** Needed by `lang-sql` for proper indentation behavior. Pulling it in keeps `pnpm check` happy.
- **No theme dep needed.** Default light theme matches the rest of the UI (which is also pre-M6). Dark mode is **M6**.
- **`indentWithTab` is one binding.** Without it, Tab in the editor moves focus instead of inserting whitespace. Include it.
- **`bind:this={editor}` of a Svelte 5 component returns its `$bindable`/exported surface.** TypeScript shape: the imported `Editor` type has `focus()` and `setDoc(s)` as instance methods. Svelte 5 supports this with `export function`.
- **No `EditorView.lineWrapping`.** SQL is line-oriented; wrap turns wide queries into spaghetti. Users can scroll horizontally.
- **The boundary heuristic uses ASCII regex.** It correctly handles `$$tag$$` because the regex `/[A-Za-z0-9_]/` matches `tag`. Unicode tags in dollar quotes are rare in real-world Postgres — punt.
- **CodeMirror updates synchronously inside `dispatch`.** `setDoc` is safe to call from outside `$effect` — no need for `tick()`.
- **Performance.** A 10k-line query in CodeMirror is still snappy. Don't preemptively add virtualization.

## Tests

- **Add `src/lib/statement.test.ts`** (Vitest is not yet set up in Quill, so for v1 we skip automated tests). Instead, exercise via the smoke test:

### Manual smoke test

```bash
./run.sh
```

1. Connect to a Postgres. Click a DB. Verify editor shows `SELECT 1` with line numbers and PostgreSQL syntax highlighting (keywords bolded/colored).
2. Type a multi-statement buffer:
   ```sql
   -- get all users
   SELECT * FROM public.users;

   SELECT now();
   ```
3. Place cursor inside the second statement, press Ctrl+Enter (Cmd+Enter on macOS). Result `<pre>` shows the output of `SELECT now()`. A warning below the editor reads "Multiple statements detected — running only the statement at the cursor (...)".
4. Select the first statement (highlight from `SELECT` through `users`). Press Ctrl+Enter. The selection runs verbatim — no warning (selection wins).
5. Place cursor in the comment line. Press Ctrl+Enter. The statement at the cursor (the comment-bearing first statement) runs.
6. Empty the buffer. Press Ctrl+Enter. Inline warning says "Nothing to run — the buffer is empty." No invoke fires (verify in devtools).
7. Type a dollar-quoted function body and verify the `;` inside does NOT split:
   ```sql
   DO $$ BEGIN PERFORM 1; END $$;
   ```
   Run with Ctrl+Enter from anywhere inside — Postgres accepts it; no warning.
8. Undo (Ctrl/Cmd+Z) and redo (Ctrl/Cmd+Shift+Z / Cmd+Shift+Z) work.
9. Find (Ctrl/Cmd+F) opens the CodeMirror search panel.
10. Comment toggle (`Ctrl-/` on Linux, `Cmd-/` on macOS) toggles `-- ` at line start.
11. Resize the window — the editor fills its `220px` height regardless. (Resizable height is M3.6 polish.)

## Acceptance criteria

- [ ] `pnpm check` succeeds.
- [ ] `pnpm install` cleanly resolves the six new packages; `pnpm-lock.yaml` is updated.
- [ ] `git status -- src/lib/` shows `Editor.svelte` and `statement.ts` as new files.
- [ ] `grep -E "on:click|on:submit|on:keydown" src/lib/Editor.svelte` returns zero matches (Svelte 5 syntax only).
- [ ] `grep -F '$:' src/lib/Editor.svelte src/lib/statement.ts src/routes/+page.svelte` returns zero matches.
- [ ] `grep -F '<textarea' src/routes/+page.svelte` returns zero matches.
- [ ] `grep -c 'invoke<' src/lib/tauri.ts` matches M3.4's final count.
- [ ] Smoke test step 3 — multi-statement warning renders below the editor and only the cursor's statement runs.
- [ ] Smoke test step 4 — selection runs verbatim.
- [ ] Smoke test step 7 — dollar-quoted function body runs without being split.
- [ ] Default-theme editor renders with line numbers, syntax highlighting, and bracket matching.
- [ ] No new backend changes — `git diff src-tauri/` is empty for this task.
- [ ] No theme/dark-mode package — `grep '@codemirror/theme' package.json` returns zero matches.

## Out of scope

- Dark mode for the editor — **M6**.
- Schema-aware autocomplete — **M4** (the source is `@codemirror/autocomplete`; we don't even depend on it yet).
- Multiple editor instances / tabs — **M5**.
- Replacement of the `<pre>` result block by a grid — **M3.6**.
- `sqlparser-rs`-based statement boundary detection — **M4** swaps it in.
- A11y deep-dive for the editor — **M6** polish.
- Persisting editor content across reloads — **M6**.
- Cancellation UI (Cancel button) — **M3.6**.
- Load-more UX — **M3.6**.
- Multi-statement script execution — explicitly deferred per PRD §12.
- Format-on-save / format-on-Enter — explicitly deferred (PRD §12 open question; M6 will resolve).
