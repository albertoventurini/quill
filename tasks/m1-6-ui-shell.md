# M1.6 — UI shell + add-connection form

## Goal
Build the minimum Svelte UI to (a) add a saved connection, (b) connect to it, (c) run `SELECT 1`, (d) see the result as raw rows. Deliberately ugly; polish lands in M3 and M6.

## Context to read first
- `PRD.md` §7.1 (connection management fields) and §8 (layout sketch — only the split is implemented here; no real tree, editor, or grid yet).
- `AGENTS.md` — Svelte 5 (runes), TypeScript, default Prettier.
- `tasks/m1-5-tauri-commands.md` — command surface this UI calls.

## Deliverables

### 1. Layout
Two-pane split in `src/routes/+page.svelte`:
- **Left** (~280px fixed width): vertical list of saved connections + an "Add connection" button.
- **Right** (flex): a `<textarea>` for SQL (plain textarea — CodeMirror is M3), a `<input>` for "Run on database…", a Run button, and a `<pre>` for the result.

Plain CSS `display: flex`. Don't bother with resizable splitters.

### 2. State (Svelte 5 runes)
```ts
let connections   = $state<Connection[]>([]);
let selectedId    = $state<number | null>(null);
let connected     = $state<Record<number, SlotState>>({});
let runningQuery  = $state(false);
let result        = $state<QueryResult | { error: CommandError } | null>(null);
```
Load connections on mount: `connections = await api.listConnections()`.

### 3. Typed Tauri bridge
`src/lib/tauri.ts`:
```ts
import { invoke } from "@tauri-apps/api/core";

export type Connection = { id: number; name: string; host: string; port: number;
                           default_db: string; username: string; ssl_mode: string;
                           slot_budget: number; password_ref: string | null; created_at: string };
export type NewConnection = Omit<Connection, "id" | "created_at" | "password_ref">;
export type SlotInfo = { database: string; busy: boolean; last_used: string };
export type SlotState = { budget: number; slots: SlotInfo[] };
export type ColumnMeta = { name: string; type_name: string };
export type QueryResult = { columns: ColumnMeta[]; rows: unknown[][]; row_count: number; duration_ms: number };
export type CommandError = { kind: "UnknownConnection" | "NotConnected" | "Slot" | "Pg" | "Store"; message: string };

export const api = {
  listConnections:   () => invoke<Connection[]>("list_connections"),
  saveConnection:    (n: NewConnection) => invoke<Connection>("save_connection", { new: n }),
  deleteConnection:  (id: number) => invoke<void>("delete_connection", { id }),
  connectServer:     (id: number, password: string) => invoke<SlotState>("connect_server", { id, password }),
  disconnectServer:  (id: number) => invoke<void>("disconnect_server", { id }),
  runQuery:          (serverId: number, database: string, sql: string)
                       => invoke<QueryResult>("run_query", { serverId, database, sql }),
  getSlotState:      (serverId: number) => invoke<SlotState | null>("get_slot_state", { serverId }),
};
```

Mirror the Rust `CommandError` shape exactly (the serde tag is `kind`, content is `message`).

### 4. Add Connection modal
A `<dialog>` (or simple absolute-positioned `<div>`) with fields:
`name`, `host`, `port` (default 5432), `default_db`, `username`, `password` (text input for M1), `ssl_mode` (select: `disable`/`prefer`/`require`), `slot_budget` (default 2).

Submit calls `api.saveConnection(...)`, refreshes `connections`, closes modal. Password is **not** stored (that's M6); the user re-enters on Connect for M1.

### 5. Connect / disconnect
- Clicking a connection in the list selects it (`selectedId = c.id`).
- Connect button shows a password input next to it; submitting calls `api.connectServer(id, password)` and stores the returned `SlotState` in `connected[id]`.
- Disconnect button calls `api.disconnectServer(id)` and clears `connected[id]`.
- Show the slot indicator next to each connection name: `[0/2]`, `[1/2 busy]`, etc., computed from `connected[id]`.

### 6. Run query
- Run button is disabled when `selectedId` is null, the server isn't connected, the SQL is empty, or `runningQuery` is true.
- Click → `runningQuery = true`; `result = null`; call `api.runQuery(...)`; in `finally`, `runningQuery = false`.
- On success: render `columns[].name` as a header line, `JSON.stringify(rows)` line by line in the `<pre>`.
- On error (the rejection is a `CommandError`): render `error.kind`: `error.message` in red.

### 7. Manual smoke test (after build)
1. `./run.sh` (first run compiles Rust; subsequent runs are quick).
2. Click "Add connection," fill in: name=local, host=localhost, port=5432, default_db=postgres, username=postgres, password=dev, ssl_mode=disable, slot_budget=2. Save.
3. Click the connection, enter password `dev`, hit Connect — indicator shows `[0/2]`.
4. Type `SELECT 1 AS one` in the textarea, "database" = `postgres`, click Run.
5. Result pane shows the column header and a row containing `1`. Indicator becomes `[1/2]` then `[1/2 idle]`.

## Acceptance criteria
- [ ] `./run.sh` opens a window; the manual smoke test passes end-to-end against local Docker Postgres.
- [ ] Run button is disabled when it shouldn't be runnable; no crashes.
- [ ] `CommandError` rejections are surfaced legibly (kind + message), not raw stringified objects.
- [ ] `./test.sh` still passes.
- [ ] Code uses Svelte 5 runes; no legacy `$:` reactivity hacks.

## Visual quality
**Deliberately minimal.** Default browser styling + a few CSS lines for the split. Zero effort on polish; M3/M6 own that.

## Out of scope
- Connection tree (databases / schemas / tables) — M2.
- CodeMirror editor — M3.
- Result grid — M3.
- Saved / history sidebars — M5.
- Keyring password storage — M6.
- Per-server multi-tab queueing UI — later.
