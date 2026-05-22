# M4.1 — Schema payload v2 (columns per relation)

## Goal

**Before (post-M3):** `introspect::introspect_database` returns `SchemaPayload { v: 1, schemas: Vec<SchemaInfo> }` where each `RelationInfo` carries only `name` and `kind`. M4's autocomplete cannot suggest columns from this — the data simply isn't there. The session-scoped in-memory cache (`registry::ServerHandle.schema_cache`) holds the v1 payload per `(server, database)`.

**After:** `SchemaPayload.v` is bumped to `2`. `RelationInfo` gains a `columns: Vec<ColumnInfo>` field (always present, always populated — never `Option`). A single new catalog query joins `pg_attribute` to `pg_class`/`pg_namespace` and returns every column of every visible relation in one round-trip, grouped into `RelationInfo` server-side. `FunctionInfo` is **not** extended in this task — function-argument completion is explicitly v1.1 per the M4 brief.

The integration tests gain a fixture that creates a table with three columns (one nullable, one with a non-trivial type) and asserts the columns come back in `attnum` order with the right `type_name` and `not_null` flags.

This task is **backend-only**. The frontend `SchemaPayload` type and the `Tree.svelte` rendering both continue to work unchanged — they just see a `columns` field they don't read yet. M4.4 wires it into a Svelte store; M4.5 consumes it from the CodeMirror completion source.

## Current state

### `src-tauri/src/introspect/mod.rs` — the file this task edits

Read it in full before starting; the canonical types and the three internal helpers (`list_schema_names`, `list_all_relations`, `list_all_functions`) are the integration points. Key facts:

- `pub const PAYLOAD_VERSION: u32 = 1;` → bumps to `2`.
- `RelationInfo { pub name: String, pub kind: RelationKind }` → gains `pub columns: Vec<ColumnInfo>`.
- `list_all_relations` returns `Vec<(String, RelationInfo)>` joined into `SchemaInfo` by `introspect_database`. The new column query joins the same way.
- `IntrospectError::Pg(#[from] tokio_postgres::Error)` is already the right error variant for the new column query.

### `src-tauri/src/registry.rs` — session cache holds the payload

```rust
pub schema_cache: Arc<DashMap<String, SchemaPayload>>,
```

Created empty on connect, discarded on disconnect. **There is no on-disk schema cache to migrate** — the SQLite `schema_cache` table was removed in commit `de637ca` ("Replace SQLite schema cache with session-scoped in-memory cache"). The "invalidate v1 payloads on read" language in `MILESTONES.md` §M4 is moot: every session starts with an empty cache anyway. Skip the migration logic entirely.

### `src-tauri/src/commands/mod.rs` — passthrough

`ensure_payload` returns `SchemaPayload` as-is; the four `list_*` commands and `refresh_schema_cache` are unaffected — they read the same fields they always did. Adding `columns` is invisible to them.

### `src-tauri/tests/introspect_integration.rs` — the test file this task edits

The existing `introspect_database_distinguishes_table_view_matview_function` test wraps DDL in `BEGIN … ROLLBACK` so it's non-destructive. Reuse the pattern for the column test.

### `src/lib/tauri.ts` — frontend mirror

```ts
export type RelationInfo = { name: string; kind: RelationKind };
```

Gets a `columns: ColumnInfo[]` field added in this task — even though no frontend code reads it yet. Keeping the TypeScript mirror in lockstep with the Rust shape avoids a stealth divergence M4.4 would have to clean up. **This is the only frontend edit in M4.1.**

## Postgres system catalogs — one new query

### Query — columns for every visible relation

```sql
SELECT n.nspname    AS schema,
       c.relname    AS table_name,
       a.attname    AS column_name,
       format_type(a.atttypid, a.atttypmod) AS type_name,
       a.attnotnull AS not_null,
       a.attnum     AS position
FROM pg_attribute a
JOIN pg_class      c ON a.attrelid = c.oid
JOIN pg_namespace  n ON c.relnamespace = n.oid
WHERE c.relkind IN ('r', 'v', 'm', 'p')
  AND a.attnum > 0          -- exclude system columns (tableoid, cmin, etc.)
  AND NOT a.attisdropped    -- exclude tombstoned columns
  AND n.nspname NOT LIKE 'pg\_%' ESCAPE '\'
  AND n.nspname <> 'information_schema'
ORDER BY n.nspname, c.relname, a.attnum
```

- `format_type(atttypid, atttypmod)` returns the human-readable type ("integer", "varchar(255)", "numeric(10,2)", "timestamp with time zone"). Use this string as `ColumnInfo.type_name`; M4 displays it next to column completions.
- `attnum > 0` is the canonical filter for user columns. System columns (`ctid`, `oid`, `xmin`, etc.) have `attnum < 0` and are not interesting for autocomplete.
- `NOT attisdropped` skips columns that have been dropped but are still physically present in the row format (Postgres' MVCC quirk).
- `attnotnull` becomes the `not_null` bool. Don't bother with defaults / generated / identity flags in v1 — they're not used by autocomplete.

A single query returns **all columns for all visible relations**. For a stock `postgres` database this is ~10 rows; for a 500-table app schema it's a few thousand. Each row is ~80 bytes — comfortably under the few-hundred-KB-to-few-MB payload budget called out in `MILESTONES.md`.

## Deliverables

### 1. `src-tauri/src/introspect/mod.rs` — payload bump + new ColumnInfo + new internal query

Edits:

**Bump `PAYLOAD_VERSION`:**

```rust
pub const PAYLOAD_VERSION: u32 = 2;
```

**Add `ColumnInfo` next to `RelationInfo`:**

```rust
/// One column of a table / view / matview / partitioned table.
///
/// `type_name` is the human-readable Postgres type, as returned by
/// `format_type(atttypid, atttypmod)` — e.g. `"integer"`, `"varchar(255)"`,
/// `"timestamp with time zone"`.  Autocomplete displays it next to the
/// completion label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub type_name: String,
    pub not_null: bool,
    /// 1-based ordinal position within the relation, matching `pg_attribute.attnum`.
    pub position: i16,
}
```

**Extend `RelationInfo`:**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationInfo {
    pub name: String,
    pub kind: RelationKind,
    /// Columns in `attnum` order.  Always populated since v2 — for views and
    /// matviews this is the projected output columns; for partitioned tables
    /// it's the parent's declared columns (partitions inherit them).
    pub columns: Vec<ColumnInfo>,
}
```

**Add the new internal query `list_all_columns`:**

Place it next to `list_all_relations`. It returns a per-relation map keyed by `(schema, relation_name)`:

```rust
async fn list_all_columns(
    client: &Client,
) -> Result<std::collections::HashMap<(String, String), Vec<ColumnInfo>>, IntrospectError> {
    let rows = client
        .query(
            r"SELECT n.nspname    AS schema,
                     c.relname    AS table_name,
                     a.attname    AS column_name,
                     format_type(a.atttypid, a.atttypmod) AS type_name,
                     a.attnotnull AS not_null,
                     a.attnum     AS position
              FROM pg_attribute a
              JOIN pg_class      c ON a.attrelid = c.oid
              JOIN pg_namespace  n ON c.relnamespace = n.oid
              WHERE c.relkind IN ('r', 'v', 'm', 'p')
                AND a.attnum > 0
                AND NOT a.attisdropped
                AND n.nspname NOT LIKE 'pg\_%' ESCAPE '\'
                AND n.nspname <> 'information_schema'
              ORDER BY n.nspname, c.relname, a.attnum",
            &[],
        )
        .await?;

    let mut out: std::collections::HashMap<(String, String), Vec<ColumnInfo>> =
        std::collections::HashMap::new();
    for row in rows {
        let schema: String = row.try_get::<_, &str>("schema")?.to_string();
        let table: String = row.try_get::<_, &str>("table_name")?.to_string();
        let name: String = row.try_get::<_, &str>("column_name")?.to_string();
        let type_name: String = row.try_get::<_, &str>("type_name")?.to_string();
        let not_null: bool = row.try_get("not_null")?;
        let position: i16 = row.try_get("position")?;

        out.entry((schema, table)).or_default().push(ColumnInfo {
            name,
            type_name,
            not_null,
            position,
        });
    }
    Ok(out)
}
```

**Update `introspect_database` to fetch + stitch columns:**

After `list_all_relations` and before `list_all_functions`, call `list_all_columns` and merge:

```rust
pub async fn introspect_database(client: &Client) -> Result<SchemaPayload, IntrospectError> {
    let schemas = list_schema_names(client).await?;
    let mut relations = list_all_relations(client).await?;
    let mut columns_by_rel = list_all_columns(client).await?;
    let functions = list_all_functions(client).await?;

    // Splice columns into their owning relations.  Relations not present in
    // the columns map (e.g. a view that returns zero columns — unusual but
    // legal) end up with `columns: Vec::new()`.
    for (schema, rel) in relations.iter_mut() {
        let key = (schema.clone(), rel.name.clone());
        rel.columns = columns_by_rel.remove(&key).unwrap_or_default();
    }

    let mut by_schema: std::collections::BTreeMap<String, SchemaInfo> = schemas
        .into_iter()
        .map(|name| {
            (
                name.clone(),
                SchemaInfo {
                    name,
                    relations: Vec::new(),
                    functions: Vec::new(),
                },
            )
        })
        .collect();

    for (schema, rel) in relations {
        by_schema
            .entry(schema.clone())
            .or_insert_with(|| SchemaInfo {
                name: schema,
                relations: Vec::new(),
                functions: Vec::new(),
            })
            .relations
            .push(rel);
    }
    for (schema, func) in functions {
        by_schema
            .entry(schema.clone())
            .or_insert_with(|| SchemaInfo {
                name: schema,
                relations: Vec::new(),
                functions: Vec::new(),
            })
            .functions
            .push(func);
    }

    Ok(SchemaPayload {
        v: PAYLOAD_VERSION,
        schemas: by_schema.into_values().collect(),
    })
}
```

Note `relations` is now bound `mut` (it was previously consumed by-value in the loop). The order of operations matters: column-stitching has to happen *before* the relations are drained into `by_schema`.

**Update the existing unit tests:**

The `payload_round_trips_through_json` test constructs `RelationInfo` values without a `columns` field — they now need `columns: Vec::new()` (or, for one of them, a populated `Vec<ColumnInfo>` to round-trip the new field). Easiest patch:

```rust
RelationInfo {
    name: "users".into(),
    kind: RelationKind::Table,
    columns: vec![
        ColumnInfo {
            name: "id".into(),
            type_name: "integer".into(),
            not_null: true,
            position: 1,
        },
        ColumnInfo {
            name: "email".into(),
            type_name: "text".into(),
            not_null: false,
            position: 2,
        },
    ],
},
RelationInfo {
    name: "user_emails".into(),
    kind: RelationKind::View,
    columns: Vec::new(),
},
```

Add one new unit test asserting `PAYLOAD_VERSION == 2`:

```rust
#[test]
fn payload_version_is_two_in_m4() {
    assert_eq!(PAYLOAD_VERSION, 2, "M4.1 bumps the payload version");
}
```

### 2. `src-tauri/tests/introspect_integration.rs` — new test, fixture extension

The existing fixture test already creates `t1`, `v1`, `m1` inside `quill_m22_fixture`. Extend it (or add a sibling test) to give `t1` three columns and assert they come back correctly.

The simplest patch is **one new test** that reuses the same `BEGIN … ROLLBACK` pattern:

```rust
#[tokio::test]
async fn introspect_database_returns_columns_for_relations() {
    let Some(dsn) = dsn() else { skip_note(); return };
    let connector = connector_from(&dsn);
    let (mut client, _cancel) = connector.connect(&dsn.database).await.expect("connect");

    client.batch_execute("BEGIN").await.expect("begin");
    client.batch_execute("CREATE SCHEMA quill_m41_fixture").await.expect("create schema");
    client.batch_execute(
        "CREATE TABLE quill_m41_fixture.users (
             id           integer NOT NULL,
             email        text,
             signup_at    timestamp with time zone NOT NULL
         )",
    ).await.expect("create table");
    // A view with two projected columns to confirm views also report columns.
    client.batch_execute(
        "CREATE VIEW quill_m41_fixture.user_emails AS
             SELECT id, email FROM quill_m41_fixture.users",
    ).await.expect("create view");

    let payload = introspect::introspect_database(&client)
        .await
        .expect("introspect_database");

    assert_eq!(payload.v, 2, "post-M4.1 payload version must be 2");

    let schema = payload
        .schemas
        .iter()
        .find(|s| s.name == "quill_m41_fixture")
        .expect("fixture schema must appear");

    let users = schema
        .relations
        .iter()
        .find(|r| r.name == "users")
        .expect("users table must appear");

    assert_eq!(users.columns.len(), 3, "users has three columns; got {:?}", users.columns);

    // Columns must be in attnum order.
    assert_eq!(users.columns[0].name, "id");
    assert_eq!(users.columns[0].type_name, "integer");
    assert!(users.columns[0].not_null, "id is NOT NULL");
    assert_eq!(users.columns[0].position, 1);

    assert_eq!(users.columns[1].name, "email");
    assert_eq!(users.columns[1].type_name, "text");
    assert!(!users.columns[1].not_null, "email is nullable");
    assert_eq!(users.columns[1].position, 2);

    assert_eq!(users.columns[2].name, "signup_at");
    assert_eq!(users.columns[2].type_name, "timestamp with time zone");
    assert!(users.columns[2].not_null);
    assert_eq!(users.columns[2].position, 3);

    let view = schema
        .relations
        .iter()
        .find(|r| r.name == "user_emails")
        .expect("user_emails view must appear");
    assert_eq!(view.columns.len(), 2, "view reports projected columns; got {:?}", view.columns);
    assert_eq!(view.columns[0].name, "id");
    assert_eq!(view.columns[1].name, "email");

    client.batch_execute("ROLLBACK").await.expect("rollback");
}
```

The existing `introspect_database_distinguishes_table_view_matview_function` test does **not** need to change — its assertions don't touch `columns`. Leave it alone; the column field is silently populated and the assertions still pass.

### 3. `src/lib/tauri.ts` — keep the TS mirror in sync

Edit `RelationInfo` to add `columns`, and add `ColumnInfo` next to it:

```ts
export type ColumnInfo = {
  name: string;
  type_name: string;
  not_null: boolean;
  position: number;
};

export type RelationInfo = {
  name: string;
  kind: RelationKind;
  columns: ColumnInfo[];
};
```

No call site in `+page.svelte` / `Tree.svelte` reads `columns` yet, and the type widening is backwards-compatible. `pnpm check` should still pass clean.

## Implementation order

There are **no intermediate compile errors** if you follow this sequence.

1. **Edit `src-tauri/src/introspect/mod.rs`** in this order:
   1. Add `ColumnInfo` struct.
   2. Add `columns: Vec<ColumnInfo>` to `RelationInfo`.
   3. Add `list_all_columns` async helper.
   4. Patch `introspect_database` to call it and splice columns in.
   5. Bump `PAYLOAD_VERSION` to `2`.
   6. Fix the existing unit tests to populate `columns` on each `RelationInfo` literal.
   7. Add the new `payload_version_is_two_in_m4` unit test.
2. Run `( cd src-tauri && cargo build )` — should compile clean.
3. Run `( cd src-tauri && cargo test introspect )` — the four existing unit tests + the new one pass.
4. **Edit `src-tauri/tests/introspect_integration.rs`** — add the new `introspect_database_returns_columns_for_relations` test. Run `./test.sh` to confirm it skips cleanly without `QUILL_TEST_PG_URL`. With the env var set against a local Postgres, all five integration tests pass.
5. **Edit `src/lib/tauri.ts`** — add `ColumnInfo`, extend `RelationInfo`. Run `pnpm check` — clean.

## Known gotchas

- **`pg_attribute.attnotnull` is `bool`, not `i8`.** Don't cast it; `row.try_get::<_, bool>("not_null")` works directly. (Catalog bool columns are real Postgres `bool`, not the `"char"` type some other catalog flags use.)
- **`pg_attribute.attnum` is `int2` (`i16`), not `i32`.** Using `try_get::<_, i32>` returns `ColumnDecode` at runtime. Annotate as `i16`.
- **`format_type(atttypid, atttypmod)` does not double-quote schema-qualified type names.** A type like `myschema.my_enum` comes back as `myschema.my_enum`, not `"myschema"."my_enum"`. That's fine for display — the auto-quoter in M4.5 only quotes identifiers it generates, not strings it received.
- **`attisdropped` columns are physically present** but logically deleted; user code can't reference them by name. Always filter them out.
- **System columns (`attnum < 0`):** `ctid`, `oid`, `cmin`, `cmax`, `xmin`, `xmax`, `tableoid`. Postgres lets you `SELECT ctid FROM t`, but autocomplete v1 doesn't surface them. The `attnum > 0` filter is the canonical exclusion. If a user complains, the v1.1 extension is to add a `system: bool` flag and conditionally include them.
- **Views and matviews populate `pg_attribute` too** with their *projected* columns. The query treats them the same as tables — no special case. Tests cover the view path.
- **Partitioned tables (`relkind = 'p'`) inherit columns from the parent declaration.** `pg_attribute` carries them on the parent oid, so the query returns them. Individual partitions (`relkind = 'r'`) also have their own `pg_attribute` rows (identical column set) — they appear as separate `RelationInfo` entries, each with the full column list. Fine.
- **`HashMap` (not `BTreeMap`) for `columns_by_rel`** is the right call because the columns *within* a relation are still appended in `attnum` order via the SQL `ORDER BY`. The map is just a grouping bucket; insertion order into the bucket is what matters, and `entry(...).or_default().push(...)` preserves that. The map's key iteration order is irrelevant — we drain it by lookup.
- **Don't switch `list_all_relations` to fetch columns inline** via a single mega-query joined to `pg_attribute`. The N+1-ness of two queries is cheap; the readability of two simple queries is high. Keep them split.
- **`SchemaPayload` `PartialEq`/`Eq` derives still hold** since `ColumnInfo` is `Eq`. No changes needed to the test's `assert_eq!(payload, back)` round-trip.
- **`thiserror`'s `IntrospectError::Pg(#[from] tokio_postgres::Error)`** already covers any error from the new query. No new error variant required.
- **No new `Cargo.toml` deps.** Everything needed (`tokio_postgres`, `serde`, `thiserror`, `std::collections`) is already present.
- **The session in-memory cache stores `SchemaPayload` by value.** Bumping the payload version doesn't affect the in-memory layout — the cache simply holds whatever shape `introspect_database` returns. There is **no migration code**: the cache starts empty on every connect.
- **Tree.svelte renders relations as leaves without reading `columns`.** Confirmed by `grep -F columns src/lib/Tree.svelte` returning zero matches (verify after the TS edit).
- **`pnpm check` after the TS edit.** A widened type can break `svelte-check` only if existing code constructs a `RelationInfo` literal — search confirms it doesn't (`grep -F 'kind: "' src/` returns Tree-only matches that destructure from the API). If you find a literal, add `columns: []`.

## Tests

Run via `./test.sh` (and with `QUILL_TEST_PG_URL` set against a real Postgres for the integration tests). Coverage:

**Unit tests (always run):**
- Existing four unit tests in `introspect::tests` — adjusted to construct `RelationInfo` with `columns`. Still passing.
- New `payload_version_is_two_in_m4` — guards the version bump.

**Integration tests (skipped without `QUILL_TEST_PG_URL`):**
- All four existing tests in `introspect_integration.rs` continue to pass — `introspect_database_returns_public_schema_with_v1_payload` asserts `payload.v == PAYLOAD_VERSION`, so it picks up the bump automatically.
- New `introspect_database_returns_columns_for_relations`:
  - Creates a transient schema with one table (3 typed columns, one nullable) and one view (2 projected columns).
  - Asserts `payload.v == 2`.
  - Asserts column count, names, types (`integer`, `text`, `timestamp with time zone`), `not_null`, and `position` ordering on the table.
  - Asserts the view's projected columns appear.
  - Rolls back so the test is non-destructive.

**Frontend:**
- `pnpm check` — clean. No new behaviour to smoke-test in this task.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `./test.sh` succeeds without `QUILL_TEST_PG_URL` — all five integration tests skip; all five unit tests pass.
- [ ] `QUILL_TEST_PG_URL=... ./test.sh` succeeds against a local Postgres — all five integration tests report `ok`.
- [ ] `grep -n "PAYLOAD_VERSION" src-tauri/src/introspect/mod.rs` shows the constant set to `2` and used at least once inside `introspect_database`.
- [ ] `grep -c "pub async fn" src-tauri/src/introspect/mod.rs` returns `2` — still exactly `list_databases` and `introspect_database`; `list_all_columns` is private.
- [ ] `grep -n "ColumnInfo" src-tauri/src/introspect/mod.rs` shows the new struct definition and the `Vec<ColumnInfo>` field on `RelationInfo`.
- [ ] `grep -n "format_type" src-tauri/src/introspect/mod.rs` shows the catalog query.
- [ ] `grep -n "ColumnInfo" src/lib/tauri.ts` shows the mirror type, and `RelationInfo` includes `columns`.
- [ ] `pnpm check` succeeds clean.
- [ ] `grep -RIn "schema_cache" src-tauri/src/ migrations/` shows only the registry's in-memory `DashMap` — no SQLite table reintroduced.
- [ ] No new dependencies in `Cargo.toml` or `package.json`.
- [ ] `git diff --stat` touches at most three files: `src-tauri/src/introspect/mod.rs`, `src-tauri/tests/introspect_integration.rs`, `src/lib/tauri.ts`.

## Out of scope

- `search_path` capture — **M4.2**.
- `sqlparser-rs` integration — **M4.3**.
- Function argument metadata (`pg_proc.proargtypes`, `proargnames`, etc.) — **explicitly deferred** to v1.1 per the M4 brief. `FunctionInfo` does **not** gain new fields here.
- Default values, generated/identity flags, check constraints — not needed for autocomplete; not surfaced.
- Index / sequence / foreign-table metadata — non-goal for v1 (see `PRD.md` §3).
- Re-introspecting existing cached payloads — moot; the cache is session-scoped and starts empty.
- Tree UI changes to show columns inline — **out of scope for M4** entirely. Columns live in the autocomplete source only; the tree continues to stop at table-level leaves. (A future task can expand a table to list columns.)
- Any frontend code reading `columns` — **M4.4** (store) and **M4.5** (CodeMirror source).
