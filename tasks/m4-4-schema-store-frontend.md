# M4.4 — Frontend schema-payload store + `get_schema_payload` Tauri command

## Goal

**Before (post-M4.3):** The frontend can ask the backend "what kind of completion does the cursor want?" (`analyze_completion`) and the backend already caches `SchemaPayload` in `ServerHandle.schema_cache` indexed by `(server_id, database)`. But the **frontend** has no in-process copy of that payload — the existing `api.listSchemas/Relations/Functions` calls each return a thin slice (names, kinds, columns), never the full payload. The M4.5 completion source needs `payload.search_path` (M4.2), the full schema → relations → columns tree (M4.1), and the function list — *all in one place*, *reachable synchronously after a one-time async load*.

**After:** A new Tauri command `get_schema_payload(server_id, database) -> SchemaPayload` returns the cached payload in one round trip; on miss it falls through to `ensure_payload` (the same code path the existing `list_*` commands use), so the very first call against an uncached DB triggers a single introspection. A new frontend module `src/lib/schemaStore.ts` owns a process-wide `Map<"<serverId>:<database>", Promise<SchemaPayload>>` cache. `tree.ts`'s `clearDatabaseSubtree` and `+page.svelte`'s Disconnect handler both call into the store to keep cache lifetimes in sync with the tree state. M4.5 is the first consumer; M4.4 ships no UI behaviour — `pnpm check` and the smoke test are the only acceptance signals.

## Current state

### Backend command surface (post-M4.3)

```
list_databases
list_schemas
list_relations
list_functions
refresh_schema_cache
analyze_completion       <-- M4.3 added
```

`ensure_payload` is the private helper used by `list_schemas/relations/functions`. It's already cache-backed and slot-aware (acquires a slot only on miss). M4.4 exposes it through a public command without changing its body.

### `src-tauri/src/commands/mod.rs` — `ensure_payload`

```rust
async fn ensure_payload(
    server_id: i64,
    database: &str,
    registry: &ServerRegistry,
) -> Result<SchemaPayload, CommandError> {
    // ... cache lookup, fall through to run_introspection on miss ...
}
```

This is the exact body M4.4 wants to expose; the new command is a four-line shim.

### Frontend cache today

`src/lib/tree.ts` keeps tree-node-shaped data on each `DatabaseNode` / `SchemaNode`. It is **not** a flat keyed cache and it discards data when the user collapses a branch (`children: null` is the canonical "cold" state). For completion the source needs `(server, db) → SchemaPayload` keyed lookups that survive tree-collapse — a distinct concern from the tree's UI state. **Don't** repurpose the tree's structures.

### Disconnect path

`+page.svelte`'s context-menu handler calls:

```ts
async function disconnect(serverId: number) {
  await api.disconnectServer(serverId);
  // ...tree cleanup...
}
```

After M4.4, this path also drops all `schemaStore` entries for `serverId`.

### Refresh path

`+page.svelte`'s context-menu Refresh handler calls:

```ts
await api.refreshSchemaCache(serverId, database);
clearDatabaseSubtree(databaseNode);
```

`refreshSchemaCache` already evicts the backend's in-memory cache for the DB. M4.4 also evicts the frontend store entry so the next completion trigger re-fetches.

## Design choices baked into this spec

- **One Tauri call per `(server, database)`, not four.** `get_schema_payload` returns the full `SchemaPayload` in a single round-trip. Stitching three or four `list_*` calls in the frontend would be slower and inconsistent (each call could individually hit-or-miss).
- **Promise-keyed cache, not value-keyed.** The store holds `Promise<SchemaPayload>` rather than `SchemaPayload | undefined`. Concurrent calls during the first load share the same in-flight promise (the natural request-deduping property of promise caches). On rejection the entry is *removed* so retry works.
- **Module-level `Map`, not a Svelte store.** Consumers (CodeMirror's completion source) are imperative callbacks, not reactive components. A reactive store buys nothing here and adds runes-vs-classic-store noise. If a future UI surface wants reactivity on top, wrap this in a `$state`.
- **No background prefetch.** The completion source asks for the payload on the first trigger; until then the cache stays empty. Aligns with AGENTS.md principle 1.
- **No TTL / no auto-invalidation.** The backend cache is session-scoped (cleared on disconnect); the frontend store mirrors that. Refresh is the only invalidation path during a session.
- **Explicit clear functions.** `clearSchemaPayload(serverId, database)` and `clearServerSchemaPayloads(serverId)` — called from the existing tree-refresh and disconnect handlers, respectively. Don't hide invalidation behind a "smart" auto-tracker.
- **Errors propagate through the promise.** Callers catch with `.catch(errorMessage)` or wrap in `try/await`. The store doesn't model an error state itself — the next call re-tries automatically.
- **`payload.search_path` and the full `schemas` tree are available together.** This is the only place in the frontend that needs both — anything else (the tree, the result grid) is fine reading slimmer projections via the existing commands.

## Deliverables

### 1. `src-tauri/src/commands/mod.rs` — new `get_schema_payload` command

Add next to the existing `list_*` commands. Body is a one-liner that delegates to `ensure_payload`:

```rust
/// Return the full schema payload for `(server_id, database)`.
///
/// On cache miss this acquires a slot and runs a full introspection (same
/// path as `list_schemas`).  On hit it returns the cached payload at zero
/// slot cost.  The frontend's `schemaStore` is the primary consumer.
#[tauri::command]
pub async fn get_schema_payload(
    server_id: i64,
    database: String,
    registry: State<'_, ServerRegistry>,
) -> Result<SchemaPayload, CommandError> {
    ensure_payload(server_id, &database, &registry).await
}
```

### 2. `src-tauri/src/lib.rs` — register the command

```rust
.invoke_handler(tauri::generate_handler![
    // ... existing ...
    commands::get_schema_payload,
])
```

### 3. `src/lib/tauri.ts` — TS binding

Add to the `api` object:

```ts
getSchemaPayload: (serverId: number, database: string) =>
  invoke<SchemaPayload>("get_schema_payload", { serverId, database }),
```

The `SchemaPayload` type already exists (M4.1 + M4.2 set the shape).

### 4. `src/lib/schemaStore.ts` — new module

This is the central piece. ~80–110 lines including comments and a couple of small unit-style tests at the bottom (or skip the tests — no Vitest is configured yet; the smoke test is sufficient).

```ts
//! Frontend schema-payload cache, keyed by (serverId, database).
//!
//! Lifetime model:
//!   - First call:   fires `api.getSchemaPayload`; stores the promise.
//!   - Subsequent calls (same key, while in-flight or settled OK):
//!                   return the cached promise.
//!   - On rejection: removes the entry so the next call retries.
//!   - On Refresh:   `clearSchemaPayload` evicts the entry; next call refetches.
//!   - On Disconnect: `clearServerSchemaPayloads` evicts every entry for the
//!                   server.
//!
//! This module is the only place that talks to `api.getSchemaPayload`.  The
//! CodeMirror completion source (M4.5) consumes the promise directly.

import { api, type SchemaPayload } from "./tauri";

function key(serverId: number, database: string): string {
  return `${serverId}:${database}`;
}

/** In-flight or settled promises, keyed by `${serverId}:${database}`. */
const cache = new Map<string, Promise<SchemaPayload>>();

/** Return the schema payload for (serverId, database).
 *
 *  - First invocation per key fires a single Tauri request.
 *  - Concurrent invocations share the same in-flight promise.
 *  - Rejection drops the entry; the next call refetches.
 */
export function getSchemaPayload(
  serverId: number,
  database: string,
): Promise<SchemaPayload> {
  const k = key(serverId, database);
  const existing = cache.get(k);
  if (existing) return existing;

  const promise = api.getSchemaPayload(serverId, database).catch((e) => {
    // Drop the entry so the next call retries; rethrow so the caller sees
    // the original CommandError shape.
    if (cache.get(k) === promise) cache.delete(k);
    throw e;
  });
  cache.set(k, promise);
  return promise;
}

/** Evict one (serverId, database) entry.  Called from the tree's Refresh
 *  handler after the backend `refresh_schema_cache` returns. */
export function clearSchemaPayload(serverId: number, database: string): void {
  cache.delete(key(serverId, database));
}

/** Evict every entry for a server.  Called from the Disconnect handler so
 *  reconnecting later picks up fresh data. */
export function clearServerSchemaPayloads(serverId: number): void {
  const prefix = `${serverId}:`;
  for (const k of cache.keys()) {
    if (k.startsWith(prefix)) cache.delete(k);
  }
}

/** Test-only: drop everything.  Not used by app code. */
export function __resetSchemaStore(): void {
  cache.clear();
}
```

### 5. `src/lib/tree.ts` — wire the Refresh path into the store

Extend `clearDatabaseSubtree` to also evict the schema store entry. The function previously only mutated tree state — now it also clears the M4.4 cache. Keeping both responsibilities in one function makes Refresh atomic:

```ts
import { clearSchemaPayload } from "./schemaStore";

// ...

/** Clear all loaded children of a database (and below). Used by Refresh
 *  so the next expand re-reads from the (just-refreshed) cache.  Also
 *  drops the M4.4 schema-store entry for the same `(serverId, database)`
 *  so the autocomplete source picks up the refreshed payload on its next
 *  trigger.  */
export function clearDatabaseSubtree(node: DatabaseNode): void {
  node.children = null;
  node.expanded = false;
  clearSchemaPayload(node.serverId, node.name);
}
```

### 6. `src/routes/+page.svelte` — wire Disconnect into the store

Find the disconnect handler (search for `api.disconnectServer`). After it resolves, call `clearServerSchemaPayloads(serverId)`. Pattern:

```ts
import { clearServerSchemaPayloads } from "$lib/schemaStore";

// inside the disconnect handler
async function disconnect(serverId: number) {
  try {
    await api.disconnectServer(serverId);
  } catch (e) {
    // existing error handling
  }
  clearServerSchemaPayloads(serverId);
  // existing post-disconnect tree cleanup
}
```

The exact site is wherever `api.disconnectServer` is called today — `+page.svelte`'s context-menu handler. If the call is in a `.then(...)` chain, add the clear in the success branch.

## Implementation order

1. **Backend command:** add `get_schema_payload` in `commands/mod.rs` and register it in `lib.rs`. Run `( cd src-tauri && cargo build )` clean.
2. **TS API binding:** add `getSchemaPayload` to `src/lib/tauri.ts`. Run `pnpm check` clean.
3. **`src/lib/schemaStore.ts`:** new file with `getSchemaPayload`, `clearSchemaPayload`, `clearServerSchemaPayloads`, `__resetSchemaStore`. Run `pnpm check` clean.
4. **Refresh wiring:** edit `src/lib/tree.ts` to import `clearSchemaPayload` and call it from `clearDatabaseSubtree`. `pnpm check`.
5. **Disconnect wiring:** edit `src/routes/+page.svelte` to import `clearServerSchemaPayloads` and call it after `api.disconnectServer`. `pnpm check`.
6. **Smoke test:**
   - Connect to a Postgres.
   - Open devtools console; run:
     ```js
     // From the page module — needs the dev build's HMR-exposed module.
     // Easiest path: temporarily add `window.__store = schemaStore` to schemaStore.ts
     // for the smoke, then revert.
     ```
     Or simpler: rely on the M4.5 acceptance smoke; M4.4 has no user-visible behaviour of its own.

## Known gotchas

- **Cache key choice.** The composite `${serverId}:${database}` is fine because `serverId` is always a positive integer and `database` is a Postgres identifier — neither can contain `:`. If a future task introduces non-numeric server IDs, switch to a tuple-style key (`JSON.stringify([serverId, database])`).
- **Promise sharing semantics.** Storing `Promise<T>` rather than `T | undefined` means concurrent callers during the first load share *the same promise* — no duplicate Tauri invokes. This is the canonical "promise cache" pattern; don't overthink it.
- **Rejection cleanup.** If the Tauri call rejects, we remove the cache entry inside the `.catch` so subsequent calls retry. Important: only remove if it's still the same promise (`cache.get(k) === promise`) — a refresh that races with a failing load could otherwise wipe a newly-set entry.
- **No `validFor` use here.** CodeMirror's `validFor` is M4.5's concern; the store does not interact with it.
- **`api.getSchemaPayload` runs through `ensure_payload`,** which acquires a slot only on miss. The first call after Connect against a fresh DB will visibly bump the slot indicator briefly (`[1/2]` for ~50ms). Subsequent calls (same session, same DB) are zero-slot lookups. This is the same behaviour the tree already exhibits — the autocomplete trigger just exposes it earlier.
- **Backend cache is per `ServerHandle`.** Disconnecting drops the `ServerHandle` (and the cache) immediately on the backend side; the frontend store mirroring keeps the two sides consistent. Reconnecting starts fresh on both.
- **No retry-with-backoff.** First failure → store entry dropped → next call refetches. If the user has flaky connectivity, the user retries; we don't.
- **TypeScript strictness.** `pnpm check` runs `svelte-check`; the new file should pass with zero warnings. Use `import { type SchemaPayload }` (type-only import) so the runtime bundle stays lean.
- **Don't reach into `tree.ts`'s `DatabaseNode` from `schemaStore.ts`.** The store is keyed by `(serverId, database)` strings — keep it agnostic of the tree's node objects. The reverse direction (tree.ts importing from schemaStore.ts) is fine: tree.ts already owns "stuff that happens when the DB tree mutates."
- **`__resetSchemaStore` is `__`-prefixed** to signal "test/internal." If you later add Vitest, this is the hook.
- **No SSR concerns.** `schemaStore.ts` is pure JS with a module-level Map; the module is imported only by browser-side code (the Editor's completion source and `+page.svelte`). `adapter-static` is fine.
- **Avoid `$state` in `schemaStore.ts`.** The map is not reactive on purpose. If someone tries to read it inside a `$effect`, they get the snapshot at effect-creation time — not subsequent mutations. This is the intended behavior; consumers (the CodeMirror source) await the promise directly.
- **Don't preload on connect.** A new server connection should *not* trigger `getSchemaPayload(serverId, default_db)`. The autocomplete source loads on first completion trigger. This honours principle 1.
- **One-line wiring change in `clearDatabaseSubtree` is load-bearing.** It's tempting to also wipe `node.error = null` or re-fire schema cache invalidation, but `clearDatabaseSubtree` is called from the Refresh path which already does the latter via `api.refreshSchemaCache` *before* this function runs. Don't duplicate work.
- **Disconnect ordering matters.** Call `clearServerSchemaPayloads(serverId)` **after** `api.disconnectServer` resolves, not before — if the disconnect fails and we cleared early, the next autocomplete trigger would fetch from a server whose backend cache was just cleared, doing an unnecessary introspection. (In practice disconnect doesn't fail, but the ordering is the principled one.)

## Tests

No new automated tests in this task (Vitest is not configured in the repo). The wiring is exercised by the M4.5 smoke test, which depends on the store being live.

### Manual smoke test

```bash
./run.sh
```

1. Connect to a Postgres server.
2. Open the devtools Network/IPC panel (or add a `console.log` inside `getSchemaPayload`).
3. Trigger autocomplete by opening the editor and pressing `Ctrl-Space` (this requires M4.5; for M4.4-only smoke, call `getSchemaPayload(serverId, database)` from the devtools console — see the test-helper note below).
4. Observe **exactly one** `get_schema_payload` invoke per `(server, database)` per session.
5. Right-click a database in the tree → Refresh. Trigger autocomplete again on a query that targets that DB. **Exactly one** new `get_schema_payload` invoke is fired — proving the eviction wired through.
6. Right-click the server → Disconnect. Reconnect. Trigger autocomplete on the same DB. **Exactly one** new `get_schema_payload` fires — proving server-scoped eviction.
7. Trigger autocomplete on a different DB. **Exactly one** new fetch for that DB.

For step 3 you can temporarily expose the store on `window`:

```ts
// at the bottom of schemaStore.ts, for smoke only — REVERT before commit
declare global { interface Window { __schemaStore: any } }
if (typeof window !== "undefined") {
  window.__schemaStore = { getSchemaPayload, clearSchemaPayload };
}
```

Run `await window.__schemaStore.getSchemaPayload(1, "postgres")` in devtools. Strip the smoke hook before committing — `grep -F __schemaStore` should return zero matches in the final diff.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds (with or without `QUILL_TEST_PG_URL`).
- [ ] `pnpm check` succeeds clean.
- [ ] `grep -n "get_schema_payload" src-tauri/src/commands/mod.rs` shows the new command.
- [ ] `grep -n "get_schema_payload" src-tauri/src/lib.rs` shows it registered in `generate_handler!`.
- [ ] `grep -n "getSchemaPayload" src/lib/tauri.ts` shows the API method.
- [ ] `git status` shows `src/lib/schemaStore.ts` as a new file.
- [ ] `grep -n "clearSchemaPayload\|clearServerSchemaPayloads" src/lib/tree.ts src/routes/+page.svelte` shows both call sites.
- [ ] `grep -n "__schemaStore\|window\\.__" src/lib/schemaStore.ts` returns no matches (no leftover debug hook).
- [ ] Smoke test step 4 — exactly one Tauri `get_schema_payload` per `(server, db)` per session, verified via devtools or temporary logging.
- [ ] Smoke test steps 5/6 — eviction paths confirmed.

## Out of scope

- The CodeMirror completion source itself — **M4.5**.
- A reactive Svelte 5 wrapper around the store — only add if a UI surface needs to react to schema-load state. None do in v1.
- TTL-based cache invalidation — not needed; session-scoped lifetime suffices and matches the backend.
- Background prefetch (e.g. on Connect) — explicitly forbidden by AGENTS.md principle 1.
- Surfacing schema-load failures in the tree UI — the tree already shows its own load errors via `node.error`. The schema store is a parallel concern; M4.5 will surface load errors inline below the editor.
- Persisting the schema cache across app restarts — non-goal; session-scoped is the chosen model.
- Vitest setup — separate task; for now, manual smoke is the test harness.
- Migrating the four existing `list_*` commands to use `get_schema_payload` internally — they still work, the tree still calls them, no churn needed.
