# M1.6 — UI shell + add-connection form

## Goal

**Before:** The Tauri scaffold opens a window with a `greet` form and no connection to the Rust backend beyond the default `greet` command. The M1.5 command surface (`list_connections`, `save_connection`, `delete_connection`, `connect_server`, `disconnect_server`, `run_query`, `get_slot_state`) exists in Rust but has no frontend callers. There is no Svelte state management, no typed IPC bridge, and no layout beyond the scaffold boilerplate.

**After:** A typed bridge (`src/lib/tauri.ts`) mirrors every Rust type that crosses the IPC boundary. The main page (`src/routes/+page.svelte`) is a two-pane split: left pane lists saved connections with slot indicators and connect/disconnect controls, right pane has a SQL textarea, database-name input, Run button, and a `<pre>` result area. An "Add connection" modal writes new servers to the local SQLite store. The user can connect to a server (supplying a password in-process, not stored), run `SELECT 1`, and see rows. The app is deliberately ugly — M3 and M6 own polish.

## Current state

### `src/routes/+page.svelte`

The current file is the Tauri + Svelte scaffold. It will be completely rewritten.

```svelte
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let name = $state("");
  let greetMsg = $state("");

  async function greet(event: Event) {
    event.preventDefault();
    // Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
    greetMsg = await invoke("greet", { name });
  }
</script>

<main class="container">
  <h1>Welcome to Tauri + Svelte</h1>

  <div class="row">
    <a href="https://vite.dev" target="_blank">
      <img src="/vite.svg" class="logo vite" alt="Vite Logo" />
    </a>
    <a href="https://tauri.app" target="_blank">
      <img src="/tauri.svg" class="logo tauri" alt="Tauri Logo" />
    </a>
    <a href="https://svelte.dev" target="_blank">
      <img src="/svelte.svg" class="logo svelte-kit" alt="SvelteKit Logo" />
    </a>
  </div>
  <p>Click on the Tauri, Vite, and SvelteKit logos to learn more.</p>

  <form class="row" onsubmit={greet}>
    <input id="greet-input" placeholder="Enter a name..." bind:value={name} />
    <button type="submit">Greet</button>
  </form>
  <p>{greetMsg}</p>
</main>

<style>
.logo.vite:hover {
  filter: drop-shadow(0 0 2em #747bff);
}

.logo.svelte-kit:hover {
  filter: drop-shadow(0 0 2em #ff3e00);
}

:root {
  font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
  font-size: 16px;
  line-height: 24px;
  font-weight: 400;

  color: #0f0f0f;
  background-color: #f6f6f6;

  font-synthesis: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
  -webkit-text-size-adjust: 100%;
}

.container {
  margin: 0;
  padding-top: 10vh;
  display: flex;
  flex-direction: column;
  justify-content: center;
  text-align: center;
}

.logo {
  height: 6em;
  padding: 1.5em;
  will-change: filter;
  transition: 0.75s;
}

.logo.tauri:hover {
  filter: drop-shadow(0 0 2em #24c8db);
}

.row {
  display: flex;
  justify-content: center;
}

a {
  font-weight: 500;
  color: #646cff;
  text-decoration: inherit;
}

a:hover {
  color: #535bf2;
}

h1 {
  text-align: center;
}

input,
button {
  border-radius: 8px;
  border: 1px solid transparent;
  padding: 0.6em 1.2em;
  font-size: 1em;
  font-weight: 500;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  transition: border-color 0.25s;
  box-shadow: 0 2px 2px rgba(0, 0, 0, 0.2);
}

button {
  cursor: pointer;
}

button:hover {
  border-color: #396cd8;
}
button:active {
  border-color: #396cd8;
  background-color: #e8e8e8;
}

input,
button {
  outline: none;
}

#greet-input {
  margin-right: 5px;
}

@media (prefers-color-scheme: dark) {
  :root {
    color: #f6f6f6;
    background-color: #2f2f2f;
  }

  a:hover {
    color: #24c8db;
  }

  input,
  button {
    color: #ffffff;
    background-color: #0f0f0f98;
  }
  button:active {
    background-color: #0f0f0f69;
  }
}

</style>
```

### `src/app.html`

Only the `<title>` changes. The rest stays.

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <link rel="icon" href="%sveltekit.assets%/favicon.png" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Tauri + SvelteKit + Typescript App</title>
    %sveltekit.head%
  </head>
  <body data-sveltekit-preload-data="hover">
    <div style="display: contents">%sveltekit.body%</div>
  </body>
</html>
```

### `src/routes/+layout.ts`

Not modified. Shown here because the implementer must know that SSR is disabled — all Svelte code runs exclusively in the browser, so there is no `onMount` lifecycle requirement; top-level `$effect` already runs in the browser.

```ts
// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
export const ssr = false;
```

### Reference: Tauri command signatures (from `src-tauri/src/commands/mod.rs`)

These are not modified by M1.6, but the implementer must know the exact parameter names and return types. The `$lib/tauri.ts` bridge must match these exactly.

**Command parameter names** (the `invoke` second argument keys):

| Command | args |
|---|---|
| `list_connections` | `{}` |
| `save_connection` | `{ new: NewConnection }` |
| `delete_connection` | `{ id: number }` |
| `connect_server` | `{ id: number, password: string }` |
| `disconnect_server` | `{ id: number }` |
| `run_query` | `{ serverId: number, database: string, sql: string }` |
| `get_slot_state` | `{ serverId: number }` |

**Rust `CommandError` enum** — serialized with `#[serde(tag = "kind", content = "message")]`:

```rust
pub enum CommandError {
    UnknownConnection(String),  // {"kind":"UnknownConnection","message":"..."}
    NotConnected(String),       // {"kind":"NotConnected","message":"..."}
    Slot(String),               // {"kind":"Slot","message":"..."}
    Pg(String),                 // {"kind":"Pg","message":"..."}
    Store(String),              // {"kind":"Store","message":"..."}
}
```

**Rust `Connection` struct** — what `list_connections` and `save_connection` return:

```rust
pub struct Connection {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: i32,
    pub default_db: String,
    pub username: String,
    pub ssl_mode: String,
    pub slot_budget: i32,
    pub password_ref: Option<String>,
    pub created_at: String,
}
```

**Rust `NewConnection` struct** — what `save_connection` accepts under the key `new`:

```rust
pub struct NewConnection {
    pub name: String,
    pub host: String,
    pub port: i32,
    pub default_db: String,
    pub username: String,
    pub ssl_mode: String,
    pub slot_budget: i32,
    pub password_ref: Option<String>,
}
```

**Rust `SlotState` / `SlotInfo`** — returned by `connect_server` and `get_slot_state`:

```rust
pub struct SlotState {
    pub budget: usize,
    pub slots: Vec<SlotInfo>,
}

pub struct SlotInfo {
    pub database: String,        // empty string if slot is free/unbound
    pub busy: bool,
    pub last_used: SystemTime,   // serializes as {"secs_since_epoch":..., "nanos_since_epoch":...}
}
```

**Rust `QueryResult` / `ColumnMeta`** — returned by `run_query`:

```rust
pub struct QueryResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,   // Vec<Vec<serde_json::Value>>
    pub row_count: usize,
    pub duration_ms: u64,
}

pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
}
```

### `package.json` (no changes needed)

```json
{
  "name": "quill",
  "version": "0.1.0",
  "description": "",
  "type": "module",
  "scripts": {
    "dev": "vite dev",
    "build": "vite build",
    "preview": "vite preview",
    "check": "svelte-kit sync && svelte-check --tsconfig ./tsconfig.json",
    "check:watch": "svelte-kit sync && svelte-check --tsconfig ./tsconfig.json --watch",
    "tauri": "tauri"
  },
  "license": "MIT",
  "dependencies": {
    "@tauri-apps/api": "^2",
    "@tauri-apps/plugin-opener": "^2"
  },
  "devDependencies": {
    "@sveltejs/adapter-static": "^3.0.6",
    "@sveltejs/kit": "^2.9.0",
    "@sveltejs/vite-plugin-svelte": "^5.0.0",
    "svelte": "^5.0.0",
    "svelte-check": "^4.0.0",
    "typescript": "~5.6.2",
    "vite": "^6.0.3",
    "@tauri-apps/cli": "^2"
  }
}
```

`@tauri-apps/api` is already a dependency; no additions needed.

## Deliverables

### 1. `src/lib/tauri.ts` — new file

Create the `src/lib/` directory. This file is the typed IPC bridge. Every type must match the Rust serde serialization shapes exactly.

```ts
import { invoke } from "@tauri-apps/api/core";

// ── Connection types (mirrors store::Connection / store::NewConnection) ──

export type Connection = {
  id: number;
  name: string;
  host: string;
  port: number;
  default_db: string;
  username: string;
  ssl_mode: string;
  slot_budget: number;
  password_ref: string | null;
  created_at: string;
};

/** Fields for creating a new connection. `password_ref` must be `null` in
 *  M1 — the OS keychain lands in M6. */
export type NewConnection = {
  name: string;
  host: string;
  port: number;
  default_db: string;
  username: string;
  ssl_mode: string;
  slot_budget: number;
  password_ref: null;
};

// ── Slot types (mirrors slots::SlotState / slots::SlotInfo) ──

/** SystemTime serializes as a struct with two number fields. */
export type SlotInfo = {
  database: string;
  busy: boolean;
  last_used: { secs_since_epoch: number; nanos_since_epoch: number };
};

export type SlotState = {
  budget: number;
  slots: SlotInfo[];
};

// ── Query result types (mirrors commands::QueryResult / ColumnMeta) ──

/** `rows` holds `serde_json::Value` cells — null, bool, number, string,
 *  array, or object. */
export type QueryResult = {
  columns: ColumnMeta[];
  rows: unknown[][];
  row_count: number;
  duration_ms: number;
};

export type ColumnMeta = {
  name: string;
  type_name: string;
};

// ── Error type (mirrors commands::CommandError serde tagging) ──

export type CommandError = {
  kind: "UnknownConnection" | "NotConnected" | "Slot" | "Pg" | "Store";
  message: string;
};

// ── Typed API ──

/** Every method wraps `invoke` with the correct parameter names and
 *  return type.  On error, the invoke call rejects with a `CommandError`
 *  object; callers must catch and inspect `(e as CommandError).kind`. */
export const api = {
  listConnections: () =>
    invoke<Connection[]>("list_connections"),

  saveConnection: (newConn: NewConnection) =>
    invoke<Connection>("save_connection", { new: newConn }),

  deleteConnection: (id: number) =>
    invoke<void>("delete_connection", { id }),

  connectServer: (id: number, password: string) =>
    invoke<SlotState>("connect_server", { id, password }),

  disconnectServer: (id: number) =>
    invoke<void>("disconnect_server", { id }),

  runQuery: (serverId: number, database: string, sql: string) =>
    invoke<QueryResult>("run_query", { serverId, database, sql }),

  getSlotState: (serverId: number) =>
    invoke<SlotState | null>("get_slot_state", { serverId }),
};
```

### 2. `src/routes/+page.svelte` — replace with two-pane shell

Rewrite the entire file. Replace every line of the scaffold with the content below.

```svelte
<script lang="ts">
  import { api, type Connection, type NewConnection, type SlotState, type QueryResult, type CommandError } from "$lib/tauri";

  // ── Connection list ──

  let connections = $state<Connection[]>([]);
  let selectedId = $state<number | null>(null);
  let connectedState = $state<Record<number, SlotState>>({});

  // ── Add-connection modal ──

  let showAddModal = $state(false);
  let addDialog = $state<HTMLDialogElement | null>(null);
  let addForm: NewConnection = $state({
    name: "",
    host: "localhost",
    port: 5432,
    default_db: "postgres",
    username: "postgres",
    ssl_mode: "disable",
    slot_budget: 2,
    password_ref: null,
  });
  let addError = $state("");

  // ── Connect form ──

  let connectPassword = $state("");

  // ── Query form ──

  let sql = $state("SELECT 1");
  let database = $state("");
  let runningQuery = $state(false);
  let result = $state<QueryResult | { error: CommandError } | null>(null);

  // ── Load connections on mount ──

  $effect(() => {
    api.listConnections().then((c) => (connections = c));
  });

  // Derive the currently-selected connection object.
  let selected = $derived(connections.find((c) => c.id === selectedId) ?? null);

  // ── Slot indicator label ──

  function slotLabel(state: SlotState | undefined): string {
    if (!state) return "";
    const busy = state.slots.filter((s) => s.busy).length;
    return `[${busy}/${state.budget}]`;
  }

  function isConnected(id: number): boolean {
    return id in connectedState;
  }

  // ── Add connection ──

  async function saveConnection(e: Event) {
    e.preventDefault();
    addError = "";
    try {
      const conn = await api.saveConnection({ ...addForm });
      connections = await api.listConnections();
      showAddModal = false;
      addDialog?.close();
    } catch (err) {
      addError = ((err as CommandError).message) ?? String(err);
    }
  }

  function openAddModal() {
    showAddModal = true;
    // Reset form to defaults.
    addForm = {
      name: "",
      host: "localhost",
      port: 5432,
      default_db: "postgres",
      username: "postgres",
      ssl_mode: "disable",
      slot_budget: 2,
      password_ref: null,
    };
    addError = "";
  }

  // Show the dialog after the DOM node is bound.
  $effect(() => {
    if (showAddModal && addDialog) {
      addDialog.showModal();
    }
  });

  function closeAddModal() {
    showAddModal = false;
    addDialog?.close();
  }

  // ── Connect / disconnect ──

  async function connect(id: number) {
    try {
      const state = await api.connectServer(id, connectPassword);
      connectedState = { ...connectedState, [id]: state };
      connectPassword = "";
    } catch (err) {
      result = { error: err as CommandError };
    }
  }

  async function disconnect(_id: number) {
    await api.disconnectServer(_id);
    // Remove from local state.
    const next = { ...connectedState };
    delete next[_id];
    connectedState = next;
  }

  // ── Run query ──

  async function run() {
    if (!selected || !isConnected(selected.id) || !sql.trim() || runningQuery) return;

    runningQuery = true;
    result = null;
    try {
      const db = database.trim() || selected.default_db;
      result = await api.runQuery(selected.id, db, sql);
    } catch (err) {
      result = { error: err as CommandError };
    } finally {
      runningQuery = false;
    }
  }

  let canRun = $derived(
    selected !== null &&
    isConnected(selected.id) &&
    sql.trim().length > 0 &&
    !runningQuery,
  );

  // ── Render a QueryResult for the <pre> block ──

  function renderResult(r: QueryResult): string {
    if (r.columns.length === 0)
      return `(no columns)\n${r.row_count} rows, ${r.duration_ms}ms`;
    const header = r.columns.map((c) => c.name).join("\t");
    const lines = r.rows.map((row) =>
      row
        .map((cell) => {
          if (cell === null) return "NULL";
          if (typeof cell === "object") return JSON.stringify(cell);
          return String(cell);
        })
        .join("\t"),
    );
    return [header, ...lines, "", `${r.row_count} rows in ${r.duration_ms}ms`].join("\n");
  }
</script>

<div class="shell">
  <!-- ═══════ LEFT PANE ═══════ -->
  <aside class="left-pane">
    <h2>Connections</h2>

    <!-- Connection list -->
    {#if connections.length === 0}
      <p class="muted">No saved connections.</p>
    {:else}
      <ul class="conn-list">
        {#each connections as conn (conn.id)}
          <li
            class="conn-item"
            class:selected={selectedId === conn.id}
            onclick={() => { selectedId = conn.id; }}
            onkeydown={(e) => { if (e.key === "Enter") selectedId = conn.id; }}
            role="button"
            tabindex="0"
          >
            <span class="conn-name">{conn.name}</span>
            <span class="slot-badge">{slotLabel(connectedState[conn.id])}</span>
          </li>
        {/each}
      </ul>
    {/if}

    <button class="btn" onclick={openAddModal}>+ Add Connection</button>

    <!-- Connect / disconnect controls -->
    {#if selected}
      {@const sel = selected}
      <div class="connect-area">
        {#if isConnected(sel.id)}
          <p>
            Connected.
            <button class="btn btn-danger" onclick={() => disconnect(sel.id)}>
              Disconnect
            </button>
          </p>
        {:else}
          <form
            onsubmit={(e) => { e.preventDefault(); connect(sel.id); }}
            class="connect-form"
          >
            <input
              type="password"
              placeholder="Password"
              bind:value={connectPassword}
              class="input"
            />
            <button type="submit" class="btn" disabled={!connectPassword}>
              Connect
            </button>
          </form>
        {/if}
      </div>
    {/if}
  </aside>

  <!-- ═══════ RIGHT PANE ═══════ -->
  <main class="right-pane">
    {#if selected}
      <h3>{selected.name}</h3>

      <!-- Database input -->
      <label class="field">
        Database
        <input
          type="text"
          bind:value={database}
          placeholder={selected.default_db}
          class="input"
        />
      </label>

      <!-- SQL textarea -->
      <textarea
        bind:value={sql}
        class="sql-input"
        rows={8}
        placeholder="SELECT 1"
      ></textarea>

      <!-- Run -->
      <button class="btn" onclick={run} disabled={!canRun}>
        {runningQuery ? "Running…" : "Run"}
      </button>

      <!-- Result / error -->
      {#if result}
        <div class="result-area">
          {#if "error" in result}
            <pre class="error">[{result.error.kind}] {result.error.message}</pre>
          {:else}
            <pre>{renderResult(result)}</pre>
          {/if}
        </div>
      {/if}
    {:else}
      <p class="muted">Select or add a connection to get started.</p>
    {/if}
  </main>
</div>

<!-- ═══════ ADD-CONNECTION DIALOG ═══════ -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<dialog bind:this={addDialog} class="modal" onclose={closeAddModal}>
  <h2>Add Connection</h2>
  <form onsubmit={saveConnection} class="add-form">
    <label class="field">
      Name
      <input type="text" class="input" bind:value={addForm.name} required />
    </label>
    <label class="field">
      Host
      <input type="text" class="input" bind:value={addForm.host} required />
    </label>
    <label class="field">
      Port
      <input type="number" class="input" bind:value={addForm.port} min={1} max={65535} />
    </label>
    <label class="field">
      Default database
      <input type="text" class="input" bind:value={addForm.default_db} required />
    </label>
    <label class="field">
      Username
      <input type="text" class="input" bind:value={addForm.username} required />
    </label>
    <label class="field">
      SSL mode
      <select class="input" bind:value={addForm.ssl_mode}>
        <option value="disable">disable</option>
        <option value="prefer">prefer</option>
        <option value="require">require</option>
        <option value="verify-ca">verify-ca</option>
        <option value="verify-full">verify-full</option>
      </select>
    </label>
    <label class="field">
      Slot budget
      <input type="number" class="input" bind:value={addForm.slot_budget} min={1} max={16} />
    </label>

    {#if addError}
      <p class="error">{addError}</p>
    {/if}

    <div class="modal-actions">
      <button type="button" class="btn" onclick={closeAddModal}>Cancel</button>
      <button type="submit" class="btn btn-primary">Save</button>
    </div>
  </form>
</dialog>

<!-- ═══════ STYLES ═══════ -->
<style>
  /* ── Layout ── */

  .shell {
    display: flex;
    height: 100vh;
  }

  .left-pane {
    width: 280px;
    min-width: 280px;
    border-right: 1px solid #ccc;
    padding: 1rem;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .right-pane {
    flex: 1;
    padding: 1rem;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  /* ── Connection list ── */

  .conn-list {
    list-style: none;
    padding: 0;
    margin: 0;
  }

  .conn-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.4rem 0.5rem;
    cursor: pointer;
    border-radius: 4px;
    border: 1px solid transparent;
  }

  .conn-item:hover {
    background: #e8e8e8;
  }

  .conn-item.selected {
    background: #d0d0ff;
    border-color: #8888cc;
  }

  .slot-badge {
    font-size: 0.8rem;
    color: #666;
    font-variant-numeric: tabular-nums;
  }

  /* ── Common controls ── */

  .btn {
    padding: 0.4rem 0.8rem;
    border: 1px solid #888;
    border-radius: 4px;
    background: #f0f0f0;
    cursor: pointer;
    font: inherit;
  }

  .btn:hover {
    background: #e0e0e0;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .btn-primary {
    background: #3366cc;
    color: white;
    border-color: #2255aa;
  }

  .btn-primary:hover {
    background: #2255aa;
  }

  .btn-danger {
    background: #cc3333;
    color: white;
    border-color: #aa2222;
  }

  .btn-danger:hover {
    background: #aa2222;
  }

  .input {
    padding: 0.4rem;
    border: 1px solid #aaa;
    border-radius: 4px;
    font: inherit;
    box-sizing: border-box;
  }

  /* ── Connect area ── */

  .connect-area {
    border-top: 1px solid #ccc;
    padding-top: 0.75rem;
  }

  .connect-form {
    display: flex;
    gap: 0.5rem;
  }

  .connect-form .input {
    flex: 1;
  }

  /* ── SQL input ── */

  .sql-input {
    width: 100%;
    padding: 0.5rem;
    font: 14px monospace;
    border: 1px solid #aaa;
    border-radius: 4px;
    box-sizing: border-box;
    resize: vertical;
  }

  /* ── Result ── */

  .result-area {
    border-top: 1px solid #ccc;
    padding-top: 0.5rem;
  }

  .result-area pre {
    margin: 0;
    font: 13px monospace;
    white-space: pre-wrap;
  }

  .error {
    color: #cc0000;
  }

  /* ── Modal ── */

  .modal {
    border: 1px solid #888;
    border-radius: 8px;
    padding: 1.5rem;
    max-width: 400px;
    width: 90%;
  }

  .modal::backdrop {
    background: rgba(0, 0, 0, 0.3);
  }

  .add-form {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: 0.9rem;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }

  /* ── Misc ── */

  .muted {
    color: #888;
    font-style: italic;
  }

  h2, h3 {
    margin: 0 0 0.25rem 0;
    font-size: 1.1rem;
  }
</style>
```

### 3. `src/app.html` — change title

Edit exactly one line. Change:

```html
    <title>Tauri + SvelteKit + Typescript App</title>
```

to:

```html
    <title>Quill</title>
```

## Implementation order

Touch files in this order. There are no intermediate compile errors that block a later step — the frontend compiles independently of the Rust backend.

1. **`src/lib/` — create directory.** `mkdir -p src/lib`
2. **`src/lib/tauri.ts`** — write the typed bridge. This file has no dependencies on other frontend code; it only imports from `@tauri-apps/api`.
3. **`src/app.html`** — change the `<title>`. Trivial; cannot break anything.
4. **`src/routes/+page.svelte`** — replace the entire file with the two-pane shell. This imports from `$lib/tauri`, which resolves to `src/lib/tauri.ts` via SvelteKit's built-in `$lib` alias (no config needed). Run `pnpm check` to verify that the Svelte and TypeScript compile cleanly before starting the full app.

## Known gotchas

- **Tauri 2 command argument casing.** Tauri 2's `#[tauri::command]` macro applies `rename_all = "camelCase"` to top-level argument names. This means:
  - Rust `server_id` → JS invoke key `serverId` (used in `run_query`, `get_slot_state`, `connect_server` — but `connect_server` only has single-word args `id`/`password`, so the renaming has no visible effect there).
  - Struct fields *inside* a command argument (like `NewConnection` fields `default_db`, `ssl_mode`, `slot_budget`) use the struct's own serde attributes. Since `NewConnection` has no `rename_all`, these are **snake_case** in the JS object.
  - Return values use the struct's own serde attributes (no rename_all → snake_case).
  - **Summary:** invoke keys are camelCase where the Rust param has underscores; struct fields are always snake_case. A mismatch on either side produces a silent `None` deserialization or an ignored field.

- **Svelte 5 event handler syntax.** Use `onclick={handler}`, `onsubmit={handler}`, `onkeydown={handler}` — the Svelte 5 attribute syntax. Do **not** use the legacy `on:click={handler}` syntax from Svelte 4 (it still works in Svelte 5 but emits a deprecation warning and may break in a future version). All event handlers in the deliverable code use the new syntax.

- **Svelte 5 `$effect` runs in the browser, not during SSR.** Because `+layout.ts` sets `export const ssr = false`, there is no SSR pass. `$effect` runs exactly once on mount when its tracked dependencies don't change. This is equivalent to `onMount` from Svelte 4 — do not import `onMount`.

- **`ComponentRenderResult` not found / `invoke<QueryResult>` type mismatch.** The `invoke` generic `invoke<T>("cmd", args)` returns `Promise<T>`. If the Rust command returns `Err`, Tauri 2 rejects with the serialized error object **as-is** — it is NOT wrapped in `{ error: ... }`. The catch handler receives the `CommandError` directly. The frontend code must catch and construct `{ error: err as CommandError }` manually, which is what the `run` function does.

- **`CommandError` is a serde-tagged enum, not a struct.** `CommandError.Serialize` uses `#[serde(tag = "kind", content = "message")]`, so the JSON is `{"kind": "Pg", "message": "some text"}`. The TypeScript `CommandError` type uses a string union for `kind`. If the Tauri serialisation layer ever normalises error shapes (e.g. wrapping them in `{ code, message }`), the frontend will get `[object Object]` instead of a readable error. The Tauri 2.0 convention is that `Result::Err(E)` where `E: Serialize` is serialized directly as the rejection value — verify this with `pnpm check` (the types won't catch a shape mismatch at runtime; the smoke test must exercise a deliberate error, like a syntax error in SQL).

- **`password_ref` must be `null`, not omitted or `undefined`.** Tauri's deserialisation (serde_json under the hood) expects the key to be present. If the key is missing entirely from the JS object, serde treats it as `None` for `Option<T>`. However, explicitly including `password_ref: null` is clearer and prevents a silent `None` that a future maintainer might mistake for a bug. The `NewConnection` type and the `addForm` initialiser both set it to `null`.

- **`selected` is a `$derived` that depends on `$state`.** `$derived` expressions must be synchronous and refer only to reactive values declared with `$state`, `$derived`, or `$props`. The `selected` derivation (`connections.find(...)`) is legal because `connections` and `selectedId` are both `$state`.

- **Replacing `connectedState` immutably.** Svelte 5's reactivity is based on assignment. Mutating an object stored in `$state` (e.g. `connectedState[id] = state`) will NOT trigger reactivity — you must replace the entire object (`connectedState = { ...connectedState, [id]: state }`). The deliverable code does this correctly. The same applies to removing a key: `delete next[id]; connectedState = next;`.

- **`<dialog>` and `showModal()` lifecycle.** The `$effect` that calls `addDialog.showModal()` depends on `showAddModal` and `addDialog`. When the modal closes (either via the Cancel button calling `closeAddModal()` or the Escape key), `onclose` fires and calls `closeAddModal()`, which sets `showAddModal = false` and calls `addDialog.close()`. The `close()` call on an already-closed dialog is a no-op and does not throw.

- **TypeScript narrowing doesn't carry into closures with `$derived`.** `selected` is a `$derived` value typed `Connection | null`. Inside `{#if selected}`, TypeScript knows `selected` is non-null for direct expressions (`{selected.name}`), but arrow-function event handlers like `() => disconnect(selected.id)` run in a closure where narrowing is lost — `svelte-check` will flag `selected` as possibly null. The fix is `{@const sel = selected}` at the top of the block, which creates a stable binding TypeScript can narrow. The deliverable code already uses this pattern in the left-pane connect area.

- **`pnpm check` runs `svelte-kit sync` first.** This generates `.svelte-kit/tsconfig.json` and ensures the `$lib` path alias resolves. The `check` script in `package.json` already chains both commands: `"check": "svelte-kit sync && svelte-check --tsconfig ./tsconfig.json"`. Do not run `svelte-check` directly without `svelte-kit sync` on a fresh checkout or after the first `$lib` import is added.

- **`invoke` return types are erased at runtime.** `invoke<QueryResult>("run_query", ...)` returns `Promise<QueryResult>` per TypeScript, but at runtime it's `Promise<unknown>`. If the Rust side changes its serialization shape (e.g. renames a field), the mismatch won't surface until the user clicks Run. The smoke test in the acceptance criteria exercises this path.

- **`<textarea>` binding.** In Svelte 5, `bind:value={sql}` works on `<textarea>` just like on `<input>`. The `rows={8}` attribute sets the visible height; `resize: vertical` in CSS lets the user expand it.

- **No `on:keydown` on non-interactive elements.** The connection list items use `role="button"` and `tabindex="0"` to make them keyboard-accessible. The `onkeydown` handler is on the `<li>`, which is legal because the element is interactive (has `tabindex`). The `svelte-check` linter may still warn about `a11y_no_static_element_interactions`; the `<!-- svelte-ignore -->` comment on the `<dialog>` suppresses the same class of warning for the dialog. If the `<li>` triggers the warning, add `<!-- svelte-ignore a11y_no_static_element_interactions -->` above it.

## Tests

There are no automated tests for the frontend in M1. The acceptance criteria are manual smoke tests. The `./test.sh` script runs `pnpm check` which does static type-checking — that must pass.

If you want to validate the `tauri.ts` types before wiring the UI, you can create a temporary test that compiles away:

```ts
// Place in src/lib/tauri.test.ts (delete before committing — not in scope)
import { api, type CommandError } from "./tauri";

// Compile-time check: the error type is a discriminated union.
function assertNever(_: CommandError): never {
  throw new Error("unreachable");
}

// Prove the union is exhaustive (will fail to compile if a variant is added).
function checkExhaustive(e: CommandError) {
  switch (e.kind) {
    case "UnknownConnection":
    case "NotConnected":
    case "Slot":
    case "Pg":
    case "Store":
      return e.message;
    default:
      return assertNever(e);
  }
}
```

This is optional and not part of the deliverable — delete the file before considering the task done.

## Acceptance criteria

Each item must be verifiable by running a single command or observing a file.

- [ ] `pnpm check` (via `./test.sh`) succeeds with no TypeScript or Svelte errors.
- [ ] `./test.sh` (which also runs `cargo test`) succeeds — no regressions in Rust tests.
- [ ] `grep -E 'on:click|on:submit|on:change|on:input|on:keydown' src/routes/+page.svelte` returns zero matches (no legacy event syntax).
- [ ] `grep -F '$:' src/routes/+page.svelte` returns zero matches (no legacy reactive declarations — Svelte 5 runes only).
- [ ] `grep -E 'export (let|const)' src/routes/+page.svelte` returns zero matches (no legacy prop declarations — `$props()` is the Svelte 5 equivalent and is not needed here).
- [ ] `ls src/lib/tauri.ts` exists and exports `api`, `Connection`, `NewConnection`, `SlotInfo`, `SlotState`, `QueryResult`, `ColumnMeta`, `CommandError`.
- [ ] `src/app.html` `<title>` is exactly `Quill`.
- [ ] `git diff --stat src-tauri/` shows zero Rust files changed (M1.6 is frontend-only).
- [ ] Manual smoke test against a local Docker Postgres container:

  ```bash
  # Start Postgres if not running
  docker run -d --name quill-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17

  # Run the app
  ./run.sh
  ```

  1. Window opens with the two-pane layout.
  2. Click **+ Add Connection**. Fill in: name=`local`, host=`localhost`, port=`5432`, default_db=`postgres`, username=`postgres`, ssl_mode=`disable`, slot_budget=`2`. Click **Save**.
  3. The connection `local` appears in the left pane. No slot indicator shown (not yet connected).
  4. Click `local` to select it. A password input appears.
  5. Enter password `dev`, click **Connect**. Indicator shows `[0/2]`.
  6. In the right pane, the SQL textarea has `SELECT 1`. Database field is empty (uses `default_db` = `postgres`). Click **Run**.
  7. Result appears in `<pre>`: `?column?` header, a row with `1`, and `1 rows in Xms`.
  8. Type `SELECT pg_sleep(1)` in the SQL field, click Run — the button shows "Running…" and is disabled. Result appears after ~1 second.
  9. Click **Disconnect**. Indicator disappears. Run button is disabled.
  10. Close the window. No crashes on exit.

## Out of scope

- Tree browsing (databases → schemas → tables) — **M2**.
- CodeMirror editor — **M3**.
- Result grid, cancellation — **M3**.
- Autocomplete — **M4**.
- Query history, saved queries, tabs — **M5**.
- OS keychain password storage, settings, visual polish, theming — **M6**.
- Any CSS beyond what's in the `+page.svelte` `<style>` block.
- Responsive design or resizable splitters.
- Editing or deleting connections from the UI (deletion is only via the API; a UI affordance can wait until M2/M6).
- Password field in the "Add connection" modal (password is entered per-Connect in M1).
- Slot tooltips showing per-slot database/busy state — **M6**.
