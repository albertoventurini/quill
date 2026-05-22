# M4.2 — Capture `search_path` per (server, database)

## Goal

**Before (post-M4.1):** `SchemaPayload v2` contains schemas, relations, columns, and functions — everything an autocomplete source needs *except* the user's `search_path`. Without `search_path` the completion logic can't tell which schemas to consult for unqualified table references. Today the backend never asks Postgres about session settings at all.

**After:** `SchemaPayload` gains a `search_path: Vec<String>` field. The list is the user's effective search path with `"$user"` already resolved against `current_user` and **with** the implicit `pg_catalog` excluded — i.e. the schemas Postgres would look in for unqualified user objects, in priority order. The value is read **once per (server, database)** on the same connection as the rest of the introspection, via `SELECT current_schemas(false)`. It's cached for the session lifetime exactly like the rest of the payload (in `ServerHandle.schema_cache`), and refresh of that cache (`refresh_schema_cache`) re-reads it.

This task is **backend only**. The TypeScript mirror in `tauri.ts` gains the field for parity. No frontend behaviour changes yet — M4.5 is the first consumer of `search_path`.

## Current state

### `src-tauri/src/introspect/mod.rs` — post-M4.1

```rust
pub const PAYLOAD_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaPayload {
    pub v: u32,
    pub schemas: Vec<SchemaInfo>,
}
```

`introspect_database` runs four catalog queries (schemas, relations, columns, functions) over the same `Client` and stitches them together into a `SchemaPayload`. M4.2 adds a fifth query for the search path.

### `src-tauri/src/registry.rs` — unchanged

```rust
pub struct ServerHandle {
    pub slot_manager: Arc<SlotManager<PgConnector>>,
    pub schema_cache: Arc<DashMap<String, SchemaPayload>>,
}
```

The session-scoped `schema_cache` already holds the payload per database; bundling `search_path` into `SchemaPayload` means **no new map** is needed.

### `src-tauri/src/commands/mod.rs` — `ensure_payload` already covers this

`ensure_payload` is the single chokepoint: cache miss → introspect → cache → return. Once `introspect_database` returns the new field, the cache, every `list_*` command, and `refresh_schema_cache` all transparently honour it. No command-layer edits.

### `src/lib/tauri.ts` — TS mirror

`SchemaPayload` in TS lacks `search_path`. Adding it keeps the TypeScript view honest; no consumer reads it yet.

## Why `current_schemas(false)`, not `SHOW search_path`

The M4 brief mentions `SHOW search_path`, but the wire-level command is a means; what we need is the **resolved** schema list. Two reasons to prefer `current_schemas(false)`:

1. **`$user` is resolved server-side** against `current_user`, including the case where no schema with that name exists (it's silently dropped). Doing the substitution client-side requires parsing the comma-separated string, handling quoted identifiers (`"$user"`, `"Mixed Case"`), and calling `current_user` separately to substitute — three rounds of error-prone work for no benefit.
2. **`pg_catalog` is excluded** with the `false` argument. The implicit prepending of `pg_catalog` to every `search_path` is a Postgres invariant, but autocomplete should never suggest `pg_catalog` as a schema. `current_schemas(false)` gives exactly the user-visible list.

Return type is `name[]` (`text[]`); tokio-postgres decodes it as `Vec<String>`.

A typical default value on a stock cluster:

```
postgres=> SELECT current_schemas(false);
   current_schemas
----------------------
 {public}
```

With a per-user schema and a custom path:

```
postgres=> SET search_path = "$user", common, public;
postgres=> SELECT current_schemas(false);
   current_schemas
-----------------------------
 {alberto,common,public}
```

(Only schemas that actually exist appear; `$user` is dropped if there's no schema named `alberto`.)

## Deliverables

### 1. `src-tauri/src/introspect/mod.rs` — add the field + fetch helper

**Extend `SchemaPayload`:**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaPayload {
    /// Wire-shape version.  Matches `PAYLOAD_VERSION` at write time.
    pub v: u32,
    pub schemas: Vec<SchemaInfo>,
    /// Resolved `search_path` for the connecting user against this database.
    ///
    /// - `$user` is already substituted with `current_user`.
    /// - The implicit `pg_catalog` prefix is **excluded** — only user-visible
    ///   schemas appear.
    /// - Schemas listed in `search_path` that do not exist are silently
    ///   dropped (Postgres' own semantics — see `current_schemas`).
    /// - Order matches priority: the first schema with a matching object
    ///   wins for unqualified references.
    pub search_path: Vec<String>,
}
```

**Add the fetch helper next to the other internals:**

```rust
/// Read the connecting user's effective `search_path` for the current
/// database.  Uses `current_schemas(false)` so `$user` is server-resolved
/// and `pg_catalog` is excluded.
async fn fetch_search_path(client: &Client) -> Result<Vec<String>, IntrospectError> {
    let row = client
        .query_one("SELECT current_schemas(false) AS path", &[])
        .await?;
    let path: Vec<String> = row.try_get("path")?;
    Ok(path)
}
```

`tokio_postgres` decodes Postgres `name[]` / `text[]` arrays as `Vec<String>` out of the box — no feature flag needed.

**Call it from `introspect_database`:**

After the four existing fetches (schemas/relations/columns/functions), call `fetch_search_path` and include it in the constructed payload:

```rust
pub async fn introspect_database(client: &Client) -> Result<SchemaPayload, IntrospectError> {
    let schemas = list_schema_names(client).await?;
    let mut relations = list_all_relations(client).await?;
    let mut columns_by_rel = list_all_columns(client).await?;
    let functions = list_all_functions(client).await?;
    let search_path = fetch_search_path(client).await?;

    for (schema, rel) in relations.iter_mut() {
        let key = (schema.clone(), rel.name.clone());
        rel.columns = columns_by_rel.remove(&key).unwrap_or_default();
    }

    let mut by_schema: std::collections::BTreeMap<String, SchemaInfo> = schemas
        .into_iter()
        .map(|name| (
            name.clone(),
            SchemaInfo { name, relations: Vec::new(), functions: Vec::new() },
        ))
        .collect();

    for (schema, rel) in relations {
        by_schema
            .entry(schema.clone())
            .or_insert_with(|| SchemaInfo { name: schema, relations: Vec::new(), functions: Vec::new() })
            .relations
            .push(rel);
    }
    for (schema, func) in functions {
        by_schema
            .entry(schema.clone())
            .or_insert_with(|| SchemaInfo { name: schema, relations: Vec::new(), functions: Vec::new() })
            .functions
            .push(func);
    }

    Ok(SchemaPayload {
        v: PAYLOAD_VERSION,
        schemas: by_schema.into_values().collect(),
        search_path,
    })
}
```

**Update the existing unit test `payload_round_trips_through_json`:**

Add `search_path: vec!["public".into()]` (or `Vec::new()` — either works for the round-trip assertion):

```rust
let payload = SchemaPayload {
    v: PAYLOAD_VERSION,
    schemas: vec![ /* unchanged */ ],
    search_path: vec!["public".into()],
};
```

Add one new unit test that round-trips a multi-entry path:

```rust
#[test]
fn payload_search_path_round_trips_through_json() {
    let payload = SchemaPayload {
        v: PAYLOAD_VERSION,
        schemas: Vec::new(),
        search_path: vec!["alberto".into(), "common".into(), "public".into()],
    };
    let s = serde_json::to_string(&payload).expect("serialize");
    let back: SchemaPayload = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(payload.search_path, back.search_path);
}
```

### 2. `src-tauri/tests/introspect_integration.rs` — assert search_path comes back

The fixture tests already manage a transient schema via `BEGIN … ROLLBACK`. Add **one new test** that asserts the default `search_path` on a stock database, plus one that flips it inside the transaction and verifies the resolved list reflects the new value.

```rust
#[tokio::test]
async fn introspect_database_captures_default_search_path() {
    let Some(dsn) = dsn() else { skip_note(); return };
    let connector = connector_from(&dsn);
    let (client, _cancel) = connector.connect(&dsn.database).await.expect("connect");

    let payload = introspect::introspect_database(&client)
        .await
        .expect("introspect_database");

    // Stock Postgres always has at least `public` on the path.
    assert!(
        payload.search_path.iter().any(|s| s == "public"),
        "default search_path should include `public`; got {:?}",
        payload.search_path,
    );
    // pg_catalog is implicit and must NOT appear (we called current_schemas(false)).
    assert!(
        !payload.search_path.iter().any(|s| s == "pg_catalog"),
        "pg_catalog must be excluded from search_path; got {:?}",
        payload.search_path,
    );
}

#[tokio::test]
async fn introspect_database_resolves_dollar_user_in_search_path() {
    let Some(dsn) = dsn() else { skip_note(); return };
    let connector = connector_from(&dsn);
    let (client, _cancel) = connector.connect(&dsn.database).await.expect("connect");

    // Create a schema named after the connecting user so `$user` resolves
    // to it.  ROLLBACK at the end.
    client.batch_execute("BEGIN").await.expect("begin");
    client
        .batch_execute(&format!(
            "CREATE SCHEMA \"{}\"",
            dsn.username.replace('"', "\"\"")
        ))
        .await
        .expect("create user schema");
    client
        .batch_execute("SET search_path = \"$user\", public")
        .await
        .expect("set search_path");

    let payload = introspect::introspect_database(&client)
        .await
        .expect("introspect_database");

    assert_eq!(
        payload.search_path,
        vec![dsn.username.clone(), "public".to_string()],
        "search_path must have $user resolved to {} and exclude pg_catalog",
        dsn.username,
    );

    client.batch_execute("ROLLBACK").await.expect("rollback");
}
```

The existing integration tests do not need adjustment — none of them inspect `search_path`, and the field is silently populated.

### 3. `src/lib/tauri.ts` — keep the TS mirror in sync

Edit `SchemaPayload`:

```ts
export type SchemaPayload = {
  v: number;
  schemas: SchemaInfoPayload[];
  search_path: string[];
};
```

No call site reads `search_path` yet; `pnpm check` should still pass clean.

## Implementation order

1. **Edit `src-tauri/src/introspect/mod.rs`** — add the `search_path` field to `SchemaPayload`, the `fetch_search_path` helper, and the call site in `introspect_database`. Patch the existing `payload_round_trips_through_json` unit test to construct the new field, and add the new `payload_search_path_round_trips_through_json` test.
2. Run `( cd src-tauri && cargo build )` — should compile clean.
3. Run `( cd src-tauri && cargo test introspect )` — unit tests pass.
4. **Edit `src-tauri/tests/introspect_integration.rs`** — add the two new tests. Run `./test.sh` to confirm they skip cleanly without `QUILL_TEST_PG_URL`. With the env var set against a local Postgres, all seven integration tests pass.
5. **Edit `src/lib/tauri.ts`** — extend `SchemaPayload`. Run `pnpm check` — clean.

## Known gotchas

- **`current_schemas(false)` returns `name[]`, not `text[]`.** tokio-postgres decodes both as `Vec<String>` with `with-serde_json-1` feature, which is already enabled. If the decode fails at runtime with a column-type mismatch, double-check the feature is on. (Spoiler: it is — verified in `Cargo.toml` line 28-32.)
- **`current_schemas(true)` would include `pg_catalog`** at the front. **Don't use `true`.** The autocomplete should never suggest `pg_catalog` because the user doesn't think of its contents as "in scope" — they're system functions Postgres adds invisibly. If a user really wants `pg_catalog.pg_class`, they qualify explicitly.
- **Schemas in `search_path` that don't exist are silently dropped by Postgres.** This means `current_schemas(false)` is **already filtered to existing schemas**. The autocomplete logic doesn't need to reconcile the path against the schema list — every entry is guaranteed to be present in `payload.schemas` (or in some implicit system schema you can ignore).
- **The order of `search_path` matters for autocomplete ranking.** Earlier schemas have higher priority for unqualified name resolution. Preserve the order verbatim — don't sort or dedupe. (Duplicates within `search_path` are legal in Postgres but Postgres dedupes them; `current_schemas` returns deduped.)
- **Quoted identifiers like `"Mixed Case"` come back unquoted in `Vec<String>`.** The array contents are raw schema names, so a `current_schemas(false)` result like `{My Schema, public}` is a `Vec<String>` of `["My Schema", "public"]`. M4.5's auto-quoter is responsible for re-quoting when generating completions.
- **`current_user` vs `session_user`:** Postgres' `$user` substitution uses `current_user`, which honours `SET ROLE`. `current_schemas` does the same. Don't second-guess this — Postgres' rules are what users expect.
- **Why one query, not bundled with `SET application_name` etc.:** `application_name` is set at connect time in `pg/mod.rs`. The introspection module reads things, it doesn't change session state. Keep `fetch_search_path` a pure read.
- **What if `current_schemas(false)` returns NULL?** It doesn't — it returns an empty array when nothing is on the path. Postgres' type system never lets this column be NULL. The `try_get::<_, Vec<String>>` decode is safe.
- **No new dependencies.** Everything used (`tokio_postgres::Client::query_one`, `serde`, `thiserror`) is already in the dep set.
- **`refresh_schema_cache` automatically refreshes `search_path`.** Because the field lives in `SchemaPayload`, eviction of the cache row also evicts the cached path. The next `list_schemas` call (or anything that funnels through `ensure_payload`) re-introspects everything together. This is the right semantics: when the user explicitly asks for a refresh, they want fresh schemas *and* a fresh path. (If they `SET search_path` mid-session and want autocomplete to honour the new path, Refresh does it.)
- **`IntrospectError::Pg(#[from] tokio_postgres::Error)`** already covers the new query — no new error variant.
- **`query_one` vs `query`.** `query_one` errors if zero or more than one row comes back. `current_schemas(false)` always returns exactly one row, so `query_one` is correct and clearer than indexing into a `Vec`.
- **`current_user` in the test:** `dsn.username` is the libpq-style username Quill connects as; Postgres maps it to `current_user` directly (unless `SET ROLE` is in play, which our tests never do). Comparing `search_path` against `dsn.username` is correct.
- **Don't try to parse `SHOW search_path` output yourself.** Once you start writing a comma-splitter that handles quoted commas, you've reinvented `current_schemas` poorly.

## Tests

Run via `./test.sh` (and with `QUILL_TEST_PG_URL` set against a real Postgres for the integration tests). Coverage:

**Unit tests (always run):**
- `payload_round_trips_through_json` — patched to populate `search_path`; still passing.
- New `payload_search_path_round_trips_through_json` — three-entry path round-trip.
- All other unit tests from M4.1 unchanged.

**Integration tests (skipped without `QUILL_TEST_PG_URL`):**
- All six existing integration tests still pass (the new field is silently populated; their assertions don't touch it).
- New `introspect_database_captures_default_search_path` — asserts `public ∈ search_path` and `pg_catalog ∉ search_path` on a stock database.
- New `introspect_database_resolves_dollar_user_in_search_path` — creates a schema named after the connecting user inside `BEGIN…ROLLBACK`, sets `search_path = "$user", public`, and asserts `payload.search_path == [user, "public"]`.

**Frontend:**
- `pnpm check` — clean.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds without `QUILL_TEST_PG_URL` — all seven integration tests skip; all six unit tests pass.
- [ ] `QUILL_TEST_PG_URL=... ./test.sh` succeeds — all seven integration tests report `ok`.
- [ ] `grep -n "search_path" src-tauri/src/introspect/mod.rs` shows the field declaration, the `fetch_search_path` helper, and the call inside `introspect_database`.
- [ ] `grep -n "current_schemas" src-tauri/src/introspect/mod.rs` shows the one SQL query.
- [ ] `grep -n "search_path" src/lib/tauri.ts` shows the field on `SchemaPayload`.
- [ ] `grep -c "pub async fn" src-tauri/src/introspect/mod.rs` returns `2` — `fetch_search_path` is private.
- [ ] `grep -n "PAYLOAD_VERSION" src-tauri/src/introspect/mod.rs` still shows the constant at `2` — no second bump.
- [ ] No new `Cargo.toml` or `package.json` deps.
- [ ] `git diff --stat` touches at most three files: `src-tauri/src/introspect/mod.rs`, `src-tauri/tests/introspect_integration.rs`, `src/lib/tauri.ts`.
- [ ] No new Tauri command and no new registry field — the existing `list_schemas` / `refresh_schema_cache` continue to drive cache population.

## Out of scope

- A dedicated `get_search_path` Tauri command — the field rides on `SchemaPayload` so M4.4's Svelte store gets it for free from the existing `list_schemas` path.
- Capturing `search_path` per-slot (rather than per-database). Slots in Quill never call `SET search_path` themselves, so the value would be the same on every slot bound to the same database. Per-database caching is the correct granularity.
- Parsing the raw `SHOW search_path` string. We use `current_schemas(false)` precisely so we never have to.
- Watching for `SET search_path` issued by user-typed queries and invalidating the cache. v1 leaves this to the user — if they change the path and want autocomplete to follow, they Refresh. M6 can add a smarter detector if it matters.
- Function lookup by `search_path` — M4 brief defers function-argument completion to v1.1; the path is for relation lookup only.
- Frontend consumers — **M4.4** wraps the payload in a Svelte store; **M4.5** is the first reader of `search_path`.
