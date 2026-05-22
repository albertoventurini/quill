# M4.3 — `sqlparser-rs` completion-context analyzer + Tauri command

## Goal

**Before (post-M4.2):** The backend can serve schema metadata (M4.1) and the resolved `search_path` (M4.2), but it has no understanding of where the cursor sits in a SQL buffer. The frontend cannot ask "what kind of identifier is the user typing right now?" — it would have to lex SQL itself in TypeScript, which the M4 brief explicitly rejects (PRD §7.3: "Alias resolution via `sqlparser-rs`").

**After:** A new module `src-tauri/src/parse/mod.rs` exposes one public synchronous function — `analyze_completion(sql: &str, cursor: usize) -> CompletionContext` — that uses `sqlparser-rs` 0.55's PostgreSQL dialect tokenizer to:

1. Decide whether the cursor is **inside a comment or string** (→ no completions);
2. Identify the **kind** of completion the user wants (FROM-item, schema-qualified relation, table-qualified column, plain unqualified, or none);
3. Extract the **prefix** the user has typed so far (and the buffer offset where it starts, so the frontend can replace the right range);
4. Extract any **qualifier** appearing before a `.` (the schema name or table/alias name to the left of the period);
5. Walk the most recent `FROM` clause (at the cursor's paren depth) and return a list of `ScopeTable { schema?, name, alias? }` triples — the tables in scope for unqualified column completion.

The function is exposed as a `#[tauri::command]` named `analyze_completion`. It is **pure**, **sync**, **does not touch Postgres**, and is safe to call on every CodeMirror completion trigger.

This task is backend only. M4.4 wraps the command in a TypeScript binding; M4.5 is the first consumer in the CodeMirror completion source.

## Current state

### `src-tauri/Cargo.toml` — needs one new dep

`sqlparser-rs` is **not** currently in the dep set. This task adds it.

### `src-tauri/src/lib.rs` — module wiring

```rust
pub mod commands;
pub mod introspect;
pub mod pg;
pub mod query;
pub mod registry;
pub mod slots;
pub mod store;
```

Gains `pub mod parse;`. The new command is added to `tauri::generate_handler!`.

### `src-tauri/src/commands/mod.rs` — adds one tiny `#[tauri::command]`

The command itself is a one-liner — it just delegates to `parse::analyze_completion`. Placing the actual logic in `parse` keeps `commands` thin and makes the analyzer unit-testable without Tauri scaffolding.

### `src/lib/tauri.ts` — mirror the types

Frontend bindings get a `CompletionContext` type and an `analyzeCompletion(sql, cursor)` method.

## Design choices baked into this spec

- **`sqlparser-rs` ≥ 0.55, PostgreSQL dialect.** Active crate, exposed `Tokenizer` API, supports v1's dialect needs (dollar-quoted strings, identifier quoting). v0.50+ all work; pinning at `^0.55` matches the latest as of `2026-05-22`.
- **Tokenize, don't parse.** Full parsing requires a complete, syntactically valid query. Autocomplete sees partial SQL on every keystroke. Use `sqlparser::tokenizer::Tokenizer` directly; build a small token-stream analyzer on top.
- **Backwards scan from cursor.** Find the cursor position, then walk *backwards* through tokens to determine context. This is the simplest robust strategy: ignore whatever the user might type *after* the cursor; only context up to the cursor matters for what completions to suggest.
- **`scope_tables` extraction restricted to the current statement.** Top-level `;` separates statements; the analyzer treats the cursor's statement as the universe. (M3.5's `statement.ts` already does the same on the frontend; the backend duplicates the heuristic — fine, both are cheap.)
- **Subqueries in `FROM`:** the analyzer **skips** them in v1. `SELECT * FROM (SELECT id FROM users) u WHERE u.<cursor>` returns an empty `scope_tables` rather than trying to infer `u`'s columns from the inner SELECT. The M4 brief explicitly defers CTE/subquery alias resolution to v1.1.
- **Synchronous + zero I/O.** No `async`, no `tokio::spawn`, no allocation beyond what `Tokenizer` does internally. Per-call cost is microseconds; safe to invoke on every trigger.
- **Returns a fixed shape**, never errors. The `kind` field absorbs every "I couldn't figure this out" case into `CompletionKind::None`, so the frontend doesn't have to branch on a `Result`.
- **No fuzzy/ranked output.** The analyzer returns *what kind* of completion the user wants and *with which qualifier and prefix*. The actual matching against schema data is M4.5's job in the frontend.
- **The Tauri command takes `sql: String` by value.** Tauri auto-serializes from the JS side; using `String` (owned) avoids any lifetime headache. The hot path is small (≤100 KB for even huge SQL buffers) — the copy doesn't matter.

## Wire shape

The shape lives in `src-tauri/src/parse/mod.rs` and is mirrored 1-to-1 in TypeScript.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    /// Cursor inside a comment or string literal — emit no completions.
    None,
    /// FROM/JOIN item.  Suggest schemas + tables reachable via `search_path`.
    FromItem,
    /// `<ident>.<cursor>` in FROM/JOIN position — the qualifier is a schema.
    /// Suggest tables/views/matviews in that schema.
    QualifiedRelation,
    /// `<ident>.<cursor>` outside FROM/JOIN — the qualifier is a table or
    /// alias.  Suggest columns of that table/alias.
    QualifiedColumn,
    /// Plain identifier at the cursor — suggest unqualified columns from
    /// `scope_tables`, plus keywords.  When `scope_tables` is empty (no
    /// FROM clause seen yet, e.g. mid-`SELECT` list), keywords only.
    Unqualified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeTable {
    /// Schema qualifier if the user wrote `schema.table` in the FROM clause.
    pub schema: Option<String>,
    /// Bare relation name as it appeared in the FROM clause.
    pub name: String,
    /// User-supplied alias (`AS x` or bare-`x` after the relation).  When
    /// `None`, an unqualified column refers to this relation by its `name`.
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionContext {
    pub kind: CompletionKind,
    /// The identifier *before* `.` when the cursor sits in a qualified
    /// position.  `None` for `FromItem` / `Unqualified` / `None`.
    pub qualifier: Option<String>,
    /// The partial identifier the user is currently typing.  Empty string
    /// when the cursor is at whitespace or right after a `.`.
    pub prefix: String,
    /// UTF-8 byte offset in `sql` where `prefix` begins.  This is the value
    /// the frontend passes to CodeMirror's `from` in the completion result.
    pub from_offset: usize,
    /// Tables in scope per the most recent FROM clause at the cursor's
    /// paren depth.  Empty when no FROM clause precedes the cursor in the
    /// current statement.
    pub scope_tables: Vec<ScopeTable>,
}
```

## Token-by-token plan

The analyzer is one pass over the token stream up to the cursor, plus a backward walk to extract `scope_tables`.

### Step 1 — find the statement window

Cursor's statement = the slice between the last top-level `;` before the cursor and the next top-level `;` after it (or buffer end). Detection mirrors `src/lib/statement.ts`: track in-string / in-comment / dollar-quote state.

A pure-Rust mini-tokenizer that just locates `;` boundaries is cheap; alternatively, lean on `Tokenizer` over the whole buffer and filter for `Token::SemiColon`. Either works. **Recommendation:** call `Tokenizer::tokenize_with_location` once over the full buffer; the locations let us slice statements without re-tokenizing.

### Step 2 — find tokens straddling the cursor

The cursor may sit:
- Inside a `Token::Word` (a partial identifier) → walk forward to find that token's start byte and treat its slice up to cursor as `prefix`, with `from_offset = token.start_byte`.
- Inside whitespace, after a non-word token → `prefix = ""`, `from_offset = cursor`.
- Inside a `Token::SingleQuotedString` / `Token::DoubleQuotedString` (and the quote isn't closed yet) → `kind: None`, return.
- Inside a comment → `kind: None`, return.

`Tokenizer` does not preserve comments by default; pass `Tokenizer::tokenize_with_location_and_unescape(false)` and check `state` — or, simpler, use `Tokenizer::tokenize_with_location` and detect comments by re-scanning the byte range yourself. **Recommendation:** use `tokenize_with_location` and *also* track `--`-line and `/* */`-block comment spans with a custom mini-pass over the raw bytes; merge the two lookups when deciding "is the cursor in a comment?". This is ~30 lines and avoids fighting sqlparser's tokenizer settings.

### Step 3 — decide the kind

Inspect the **last non-whitespace token before the prefix start** (call it `prev`). Cases:

| `prev` token                                         | Decision                                                                            |
| ---------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `Token::Period` (and the token before is a `Word`)   | qualified context → look at the preceding non-trivial context to choose `QualifiedRelation` vs `QualifiedColumn` |
| `Token::Word { keyword: FROM | JOIN | … }`           | `FromItem`                                                                          |
| `Token::Comma` and the most recent FROM-class keyword is the nearest clause keyword before that comma | `FromItem` (continuing a table list) |
| Anything else                                        | `Unqualified`                                                                       |

Differentiating `QualifiedRelation` vs `QualifiedColumn`: scan backward from the qualifier's token, skipping over balanced parens. If the first clause keyword encountered is `FROM`/`JOIN` (and we haven't crossed a `WHERE`/`SELECT`-list/...) → `QualifiedRelation`; otherwise → `QualifiedColumn`.

Implementation tip: pre-compute, for each token in the current statement, the *index of the most recent clause keyword* (`SELECT`, `FROM`, `WHERE`, `GROUP`, `ORDER`, `HAVING`, `JOIN`-family, `ON`, `USING`). With that lookup table the decision is O(1) per call.

### Step 4 — extract `scope_tables`

Walk forward from the most-recent `FROM` in the current statement at the cursor's paren depth. Stop at the next clause keyword (`WHERE`/`GROUP`/`ORDER`/`HAVING`/`LIMIT`/`OFFSET`/`UNION`/`EXCEPT`/`INTERSECT`/`;`/end-of-input) or at any unbalanced `)`.

Within that window, recognize the table-list grammar:

```
table_list := table_ref (',' table_ref)*
table_ref  := relation [ alias ] [ join_chain ]
relation   := identifier ('.' identifier)?           -- skip if '(' (subquery)
alias      := 'AS' identifier | identifier           -- not a clause keyword
join_chain := join_kw relation [ alias ] join_clause
join_kw    := 'JOIN' | 'INNER' 'JOIN' | 'LEFT' [ 'OUTER' ] 'JOIN' | 'CROSS' 'JOIN' | ...
join_clause:= 'ON' <skip until next join_kw / comma / clause_kw / end>
            | 'USING' '(' ... ')'
```

The walker only records `(schema, name, alias)` triples — JOIN conditions, USING lists, lateral subqueries, etc. are skipped. A subquery in place of a relation (`(SELECT ...)`) is detected by `Token::LParen`; balance parens, then look for an optional alias, then continue.

### Step 5 — assemble the result

Return `CompletionContext` with all fields populated. On any irrecoverable parse glitch (e.g. tokenizer errors on a malformed `$$`), return `CompletionContext { kind: None, ... }` — the frontend treats it as "no suggestions" and the user keeps typing.

## Deliverables

### 1. `src-tauri/Cargo.toml` — add `sqlparser`

```toml
[dependencies]
# ... existing deps ...
sqlparser = "0.55"
```

`sqlparser` has no runtime feature flags relevant to this task. Default features include the PostgreSQL dialect.

### 2. `src-tauri/src/parse/mod.rs` — new module

The file is ~280–350 lines. Below is the *shape* — sample types + function signatures, plus the orchestrating `analyze_completion` body. The internal walkers (`find_statement_window`, `find_prefix_token`, `tokens_in_window`, `extract_scope_tables`, `decide_kind`) are private helpers; their internals follow the plan above. Aim for the same density as `src-tauri/src/query/mod.rs` post-M3.4 (~500 lines).

```rust
//! SQL-aware analysis of the cursor position for autocomplete.
//!
//! The single public entry point is [`analyze_completion`]: given a SQL
//! buffer and a UTF-8 byte offset, return a [`CompletionContext`] that tells
//! the frontend what kind of identifier the user is typing, what qualifier
//! (if any) precedes it, what prefix has been typed so far, and which
//! tables/aliases are in scope from the current FROM clause.
//!
//! The function uses `sqlparser-rs`'s PostgreSQL tokenizer; it does not try
//! to parse a full statement, because the buffer is almost never complete
//! at the moment the user wants a suggestion.

use serde::{Deserialize, Serialize};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer, Word};

// ---------------------------------------------------------------------------
// Wire shape
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    None,
    FromItem,
    QualifiedRelation,
    QualifiedColumn,
    Unqualified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeTable {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionContext {
    pub kind: CompletionKind,
    pub qualifier: Option<String>,
    pub prefix: String,
    pub from_offset: usize,
    pub scope_tables: Vec<ScopeTable>,
}

impl CompletionContext {
    /// "No-op" context — frontend skips completions.  Returned whenever the
    /// cursor is in a comment / string / unrecoverable lex error.
    fn none(from_offset: usize) -> Self {
        Self {
            kind: CompletionKind::None,
            qualifier: None,
            prefix: String::new(),
            from_offset,
            scope_tables: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Analyze the SQL buffer at the given byte-offset cursor and return a
/// [`CompletionContext`] describing what the frontend should suggest.
///
/// **Semantics:**
/// - Cursor inside `'…'` / `"…"` / `--…` / `/*…*/` → [`CompletionKind::None`].
/// - Cursor mid-identifier → `prefix` is the partial token, `from_offset` is
///   its start byte.
/// - Cursor at a `.`-qualified position → `qualifier` populated; `kind` is
///   [`CompletionKind::QualifiedRelation`] inside FROM/JOIN,
///   [`CompletionKind::QualifiedColumn`] elsewhere.
/// - Otherwise → [`CompletionKind::Unqualified`] or [`CompletionKind::FromItem`]
///   depending on the nearest clause keyword.
///
/// Pure, sync, microseconds per call.  Safe to invoke on every keystroke.
pub fn analyze_completion(sql: &str, cursor: usize) -> CompletionContext {
    let cursor = cursor.min(sql.len());

    // 1. Tokenize the whole buffer once.  On a tokenizer error (e.g. an
    //    unclosed dollar quote past the cursor), bail out empty.
    let dialect = PostgreSqlDialect {};
    let tokens = match Tokenizer::new(&dialect, sql).tokenize_with_location() {
        Ok(t) => t,
        Err(_) => return CompletionContext::none(cursor),
    };

    // 2. Check whether the cursor is in a comment.  sqlparser drops
    //    comment tokens from the stream, so we do a tiny dedicated scan.
    if cursor_in_comment(sql, cursor) {
        return CompletionContext::none(cursor);
    }

    // 3. Find the cursor's statement window (between top-level `;`s).
    let (stmt_start, stmt_end) = find_statement_window(&tokens, cursor, sql.len());

    // 4. Filter tokens to the statement window.
    let stmt_tokens: Vec<&TokenWithSpan> = tokens
        .iter()
        .filter(|t| in_window(t, stmt_start, stmt_end))
        .collect();

    // 5. Find the prefix-bearing token (if any) and its position.
    let prefix_info = find_prefix_token(&stmt_tokens, sql, cursor);

    // 6. Detect whether the cursor sits inside an open string literal.
    //    Tokenizer would have produced a `SingleQuotedString` covering the
    //    cursor; if so, bail out empty.
    if cursor_in_string(&stmt_tokens, cursor) {
        return CompletionContext::none(prefix_info.from_offset);
    }

    // 7. Decide the kind + qualifier.
    let (kind, qualifier) = decide_kind(&stmt_tokens, &prefix_info);

    // 8. Extract scope_tables from the most recent FROM clause in the
    //    statement window at the cursor's paren depth.
    let scope_tables = extract_scope_tables(&stmt_tokens, cursor);

    CompletionContext {
        kind,
        qualifier,
        prefix: prefix_info.prefix,
        from_offset: prefix_info.from_offset,
        scope_tables,
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

struct PrefixInfo {
    prefix: String,
    from_offset: usize,
    /// Index in `stmt_tokens` of the token whose body contains `from_offset`,
    /// or `None` if `from_offset` is in whitespace.
    prefix_token_idx: Option<usize>,
}

fn find_statement_window(
    tokens: &[TokenWithSpan],
    cursor: usize,
    sql_len: usize,
) -> (usize, usize) {
    // Walk tokens; remember the byte after the last `;` before `cursor`,
    // and the byte at the first `;` at or after `cursor`.
    let mut start = 0usize;
    let mut end = sql_len;
    for t in tokens {
        let pos = byte_offset(&t.span.start, /* compute from `sql` */ 0); // see "Known gotchas"
        if matches!(t.token, Token::SemiColon) {
            if pos < cursor {
                start = pos + 1;
            } else if pos >= cursor {
                end = pos;
                break;
            }
        }
    }
    (start, end)
}

fn in_window(t: &TokenWithSpan, start: usize, end: usize) -> bool {
    // ... uses span.start and span.end against (start, end)
    // ...
    unimplemented!()
}

fn find_prefix_token(stmt_tokens: &[&TokenWithSpan], sql: &str, cursor: usize) -> PrefixInfo {
    // Walk tokens; find the one whose span contains (or ends at) `cursor`.
    // If that token is a Word, slice from its start to `cursor` for prefix.
    // Otherwise prefix is empty and from_offset = cursor.
    unimplemented!()
}

fn cursor_in_comment(sql: &str, cursor: usize) -> bool {
    // Scan raw bytes; track --line, /* block */, and dollar-quoted strings
    // (which can legally contain `--`).  Return true if cursor sits inside
    // a -- or /* … */ region.
    unimplemented!()
}

fn cursor_in_string(stmt_tokens: &[&TokenWithSpan], cursor: usize) -> bool {
    // For each token in the statement window, check if it's a
    // SingleQuotedString / DollarQuotedString / NationalStringLiteral whose
    // span strictly contains the cursor.  (Closed strings whose end == cursor
    // do not count — the cursor is just past them.)
    unimplemented!()
}

fn decide_kind(
    stmt_tokens: &[&TokenWithSpan],
    prefix: &PrefixInfo,
) -> (CompletionKind, Option<String>) {
    // 1. Find the last non-whitespace, non-prefix token before prefix.from_offset.
    //    Call it `prev`.  Whitespace is implicit in token spans (sqlparser
    //    elides whitespace tokens but `Whitespace` is in the enum).
    // 2. If `prev` is Token::Period AND the token before that is a Word `q`:
    //       qualifier = Some(q.value)
    //       kind = if in_from_clause(stmt_tokens, period_idx) { QualifiedRelation } else { QualifiedColumn }
    // 3. Else if `prev` is a Word matching FROM/JOIN/etc.:
    //       kind = FromItem
    // 4. Else if `prev` is Token::Comma AND the comma sits inside the
    //    current FROM clause (i.e. between the FROM keyword and the next
    //    clause keyword) at the right paren depth:
    //       kind = FromItem
    // 5. Else:
    //       kind = Unqualified
    unimplemented!()
}

fn extract_scope_tables(stmt_tokens: &[&TokenWithSpan], cursor: usize) -> Vec<ScopeTable> {
    // 1. Find the most recent FROM at paren depth 0 before `cursor`.
    //    (Track paren depth as you walk; record FROM positions only at
    //    depth 0 from the cursor's perspective — i.e. the depth at the
    //    cursor must equal the depth at the FROM.)
    // 2. From that FROM, walk forward to the next clause keyword
    //    (WHERE/GROUP/ORDER/HAVING/LIMIT/OFFSET/UNION/INTERSECT/EXCEPT) or
    //    the end of the statement window or unbalanced ')'.
    // 3. Inside that span, run the table-list grammar walker described in
    //    "Token-by-token plan, Step 4."
    unimplemented!()
}
```

The exhaustive helper bodies are the engineering work for this task. Plan to write ~150 lines of helper code, ~80 lines of unit tests covering the cases below.

### 3. `src-tauri/src/parse/mod.rs` — required unit tests

The unit-test module exercises every case enumerated in the wire-shape doc above. Keep each case minimal — one assertion per test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(sql: &str) -> CompletionContext {
        let cursor = sql.find('|').expect("test SQL must contain a `|` cursor marker");
        let sql = sql.replace('|', "");
        analyze_completion(&sql, cursor)
    }

    #[test]
    fn empty_buffer_is_unqualified_with_empty_prefix() {
        let c = ctx("|");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert_eq!(c.prefix, "");
        assert_eq!(c.from_offset, 0);
        assert!(c.scope_tables.is_empty());
    }

    #[test]
    fn after_from_with_no_table_is_from_item() {
        let c = ctx("SELECT * FROM |");
        assert_eq!(c.kind, CompletionKind::FromItem);
        assert_eq!(c.prefix, "");
    }

    #[test]
    fn partial_table_after_from_is_from_item_with_prefix() {
        let c = ctx("SELECT * FROM publ|");
        assert_eq!(c.kind, CompletionKind::FromItem);
        assert_eq!(c.prefix, "publ");
        assert_eq!(c.from_offset, "SELECT * FROM ".len());
    }

    #[test]
    fn schema_qualifier_in_from_is_qualified_relation() {
        let c = ctx("SELECT * FROM public.|");
        assert_eq!(c.kind, CompletionKind::QualifiedRelation);
        assert_eq!(c.qualifier.as_deref(), Some("public"));
        assert_eq!(c.prefix, "");
    }

    #[test]
    fn partial_relation_after_schema_qualifier() {
        let c = ctx("SELECT * FROM public.us|");
        assert_eq!(c.kind, CompletionKind::QualifiedRelation);
        assert_eq!(c.qualifier.as_deref(), Some("public"));
        assert_eq!(c.prefix, "us");
    }

    #[test]
    fn alias_qualifier_outside_from_is_qualified_column() {
        let c = ctx("SELECT u.| FROM users u");
        assert_eq!(c.kind, CompletionKind::QualifiedColumn);
        assert_eq!(c.qualifier.as_deref(), Some("u"));
        assert_eq!(c.prefix, "");
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable { schema: None, name: "users".into(), alias: Some("u".into()) }],
        );
    }

    #[test]
    fn unqualified_in_select_list_with_scope() {
        let c = ctx("SELECT em| FROM users");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert_eq!(c.prefix, "em");
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable { schema: None, name: "users".into(), alias: None }],
        );
    }

    #[test]
    fn scope_includes_join_targets_with_aliases() {
        let c = ctx("SELECT | FROM users u JOIN orders AS o ON u.id = o.user_id");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert_eq!(
            c.scope_tables,
            vec![
                ScopeTable { schema: None, name: "users".into(), alias: Some("u".into()) },
                ScopeTable { schema: None, name: "orders".into(), alias: Some("o".into()) },
            ],
        );
    }

    #[test]
    fn schema_qualified_table_in_from_keeps_schema() {
        let c = ctx("SELECT | FROM common.events e");
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable { schema: Some("common".into()), name: "events".into(), alias: Some("e".into()) }],
        );
    }

    #[test]
    fn cursor_inside_line_comment_returns_none() {
        let c = ctx("SELECT 1 -- pick me |");
        assert_eq!(c.kind, CompletionKind::None);
    }

    #[test]
    fn cursor_inside_block_comment_returns_none() {
        let c = ctx("SELECT /* what| now */ 1");
        assert_eq!(c.kind, CompletionKind::None);
    }

    #[test]
    fn cursor_inside_string_literal_returns_none() {
        let c = ctx("SELECT 'hello |' FROM users");
        assert_eq!(c.kind, CompletionKind::None);
    }

    #[test]
    fn cursor_in_second_statement_uses_only_that_statements_from() {
        let c = ctx("SELECT * FROM users; SELECT | FROM orders");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable { schema: None, name: "orders".into(), alias: None }],
        );
    }

    #[test]
    fn subquery_in_from_does_not_add_to_scope_in_v1() {
        let c = ctx("SELECT | FROM (SELECT id FROM users) u");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        // v1 explicitly does NOT extract `u` from a subquery in FROM.
        assert!(c.scope_tables.is_empty());
    }

    #[test]
    fn unqualified_in_where_with_scope() {
        let c = ctx("SELECT * FROM users WHERE |");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable { schema: None, name: "users".into(), alias: None }],
        );
    }

    #[test]
    fn cursor_at_end_of_word_includes_full_word_as_prefix() {
        let c = ctx("SELECT users|");
        assert_eq!(c.prefix, "users");
        assert_eq!(c.from_offset, "SELECT ".len());
    }
}
```

Add tests for any additional edge cases you find while implementing — particularly the `Token::Period` boundary cases (`a.|`, `a. |` with a space, `.|` with no qualifier).

### 4. `src-tauri/src/lib.rs` — module + command wiring

```rust
pub mod commands;
pub mod introspect;
pub mod parse;          // <-- new
pub mod pg;
pub mod query;
pub mod registry;
pub mod slots;
pub mod store;
```

And add the command to `tauri::generate_handler!`:

```rust
.invoke_handler(tauri::generate_handler![
    // ... existing commands ...
    commands::analyze_completion,
])
```

### 5. `src-tauri/src/commands/mod.rs` — thin Tauri shim

```rust
use crate::parse::{self, CompletionContext};

/// Analyze the SQL buffer at the given UTF-8 byte offset.
///
/// Pure, sync; the `#[tauri::command]` still appears `async` to the JS
/// runtime — Tauri runs sync handlers on a blocking pool.  No Postgres
/// connection is acquired (AGENTS.md principle 1 — and there is no need:
/// the schema cache lives in the frontend per M4.4).
#[tauri::command]
pub fn analyze_completion(sql: String, cursor: usize) -> CompletionContext {
    parse::analyze_completion(&sql, cursor)
}
```

(`#[tauri::command]` accepts non-`async fn` handlers as of Tauri 2; both forms serialize the return value the same way.)

### 6. `src/lib/tauri.ts` — TS mirror + API method

```ts
export type CompletionKind =
  | "none"
  | "from_item"
  | "qualified_relation"
  | "qualified_column"
  | "unqualified";

export type ScopeTable = {
  schema: string | null;
  name: string;
  alias: string | null;
};

export type CompletionContext = {
  kind: CompletionKind;
  qualifier: string | null;
  prefix: string;
  from_offset: number;
  scope_tables: ScopeTable[];
};
```

Add to the `api` object:

```ts
analyzeCompletion: (sql: string, cursor: number) =>
  invoke<CompletionContext>("analyze_completion", { sql, cursor }),
```

## Implementation order

1. **Add `sqlparser = "0.55"` to `Cargo.toml`.** Run `( cd src-tauri && cargo build )` — pulls down the dep; no code yet.
2. **Create `src-tauri/src/parse/mod.rs`** with the wire types and a stub `pub fn analyze_completion(_: &str, _: usize) -> CompletionContext { CompletionContext::none(0) }`. Add `pub mod parse;` to `lib.rs`. Build succeeds.
3. **Write unit tests first.** Paste the test module above. They will all fail against the stub. Use them as the spec for the helpers.
4. **Implement helpers in this order:**
   1. `cursor_in_comment` — scan raw bytes.
   2. `cursor_in_string` — check token spans.
   3. `find_statement_window` — locate `;` boundaries.
   4. `find_prefix_token` — locate the partial identifier.
   5. `decide_kind` — pattern-match against last token(s).
   6. `extract_scope_tables` — walk forward from FROM.
   Run unit tests after each helper.
5. **Add the Tauri shim** in `commands/mod.rs` and register it in `lib.rs`.
6. **Add the TS mirror** in `tauri.ts`. Run `pnpm check` — clean.
7. Run `./test.sh` — all unit + integration tests pass; no new integration tests are required for this task (the analyzer is pure, no Postgres interaction).

## Known gotchas

- **`sqlparser` span types differ across versions.** In 0.55, `TokenWithSpan` has `.token: Token` and `.span: Span` where `Span { start: Location, end: Location }` and `Location { line: u64, column: u64 }`. **The tokenizer returns line/column, not byte offsets.** You need to convert to byte offsets yourself — track byte index while iterating, or use `Tokenizer::tokenize_with_location` and post-process. **Recommendation:** write a tiny helper `fn byte_offset(sql: &str, loc: Location) -> usize` that walks lines/columns; cache the per-line byte offsets if the buffer is large (it usually isn't).
- **`Token::Whitespace`** is preserved by the tokenizer. Filter it out before token-position arithmetic; otherwise "last non-whitespace token" decisions misfire.
- **`Token::Word` keyword detection.** Each `Token::Word` has `keyword: Keyword` set when the lexer recognizes a reserved word. Use this — don't string-match `value`. Important keywords for this task: `FROM`, `JOIN`, `INNER`, `LEFT`, `RIGHT`, `FULL`, `OUTER`, `CROSS`, `LATERAL`, `ON`, `USING`, `AS`, `WHERE`, `GROUP`, `BY`, `ORDER`, `HAVING`, `LIMIT`, `OFFSET`, `UNION`, `INTERSECT`, `EXCEPT`, `SELECT`, `WITH`. The `sqlparser::keywords::Keyword` enum has them all.
- **Postgres-specific dialect quirks** that the `PostgreSqlDialect` tokenizer handles: dollar-quoted strings (`$$ ... $$` and `$tag$ ... $tag$`), `E'...'` escape-string literals, `B'...'` and `X'...'` bit/hex literals, `::` cast operator. Use the Postgres dialect — not `GenericDialect` — so these all tokenize correctly.
- **`Tokenizer::tokenize_with_location` returns `Result<Vec<TokenWithSpan>, TokenizerError>`.** On an open dollar-quote, an unclosed `'`, etc., it errors. Treat that as `CompletionContext::none` and return — don't try to recover.
- **Cursor exactly at the boundary of two tokens:** e.g. `SELECT * FROM users|` — cursor at the `s`. Is the prefix `users` or empty? **`users` — when the cursor sits at the end of a Word, treat that Word's full text as the prefix.** This matches user intuition: pressing `Ctrl-Space` mid-word should complete from that word.
- **`Token::Period` between two identifiers.** sqlparser produces three tokens: `Word("a")`, `Period`, `Word("b")`. When the cursor is right after the period (`a.|`), the prefix token is empty and `prev` is `Period`. Walk back one more to find the qualifier.
- **`USING (...)` and `ON ...` join conditions** contain identifiers that *look like* table refs. The scope walker must skip past them — recognize `USING` as "skip the next `(...)` block" and `ON` as "skip until next JOIN-class keyword, comma, or clause keyword."
- **`LATERAL (...)`** is a subquery in FROM. v1 skips it.
- **Schema-qualified relation in FROM (`common.events`)** is two `Word` tokens separated by a `Period`. The walker accumulates them into `ScopeTable { schema: Some("common"), name: "events", ... }`. Triple-qualified (`db.schema.table`) doesn't happen in normal Postgres SQL — bail (treat the leading word as schema, ignore the inner one). M4 doesn't suggest the database qualifier anyway.
- **Aliasing rules.** `tbl AS x`, `tbl x`, `tbl AS "X"`, `tbl "X"` are all legal. Don't treat clause keywords (`WHERE`, `JOIN`, `ON`, `WITH`, etc.) as aliases — if the next `Word` is a keyword, the previous relation has no alias. The `Keyword::NoKeyword` sentinel on `Token::Word.keyword` flags non-reserved identifiers; alias-eligible words are exactly those with `keyword == Keyword::NoKeyword` *unless* `AS` was explicit.
- **Unicode in identifiers.** Postgres allows them (UTF-8 in quoted identifiers); `sqlparser` returns the verbatim `String`. Don't lowercase or fold — pass them through.
- **`Token::Word.quote_style: Option<char>`** is `Some('"')` when the identifier was double-quoted in the source, else `None`. The scope walker should preserve this if you want to round-trip exact identifiers — but for v1 it's enough to store the unquoted name in `ScopeTable.name`; the auto-quoter (M4.5) handles re-quoting on output.
- **`fn analyze_completion` is `pub fn`, not `pub async fn`.** The Tauri command is also sync. Don't introduce `async` — there's no I/O.
- **Don't add anyhow, eyre, color-eyre, or any error-handling crate.** The function returns `CompletionContext` directly (always-OK). Internal helpers return `Option` or use `match` directly.
- **CodeMirror's `from` is **byte- or char-offset?** CodeMirror uses **JavaScript string offsets** (UTF-16 code units). Since CodeMirror passes a string to the editor and we pass the same string through Tauri, the JS-side cursor is in UTF-16. **Tauri's serde-IPC converts JS strings to Rust `String` (UTF-8), but the cursor we receive is the UTF-16 offset.** For ASCII SQL this is identical to UTF-8; for buffers containing non-ASCII, M4.5 must convert. **Document this; v1 punts on non-ASCII identifiers in the buffer body. Quill expects mostly-ASCII SQL.** A v1.1 task can add a UTF-16↔UTF-8 conversion helper if it matters.
- **No allocation on the hot path.** `tokenize_with_location` allocates a `Vec<TokenWithSpan>` once per call. Avoid extra allocations in the helpers — borrow `&str` slices off `sql`.
- **Tests use the `|` cursor convention.** It's a convenient marker that doesn't appear in normal SQL. The test helper `ctx()` strips it.

## Tests

Run via `./test.sh`. Coverage:

**Unit tests (always run):**
- ~17 cases listed in deliverable 3 covering: empty buffer, FROM keyword, prefix in FROM, schema qualifier, qualifier with prefix, alias qualifier outside FROM, unqualified with scope, multi-table FROM with aliases, schema-qualified table in FROM, line/block comments, string literals, multi-statement scope isolation, subquery skip, WHERE-clause unqualified, cursor at end-of-word.

No new integration tests — this module is pure.

**Frontend:**
- `pnpm check` — clean. No behavioural test yet; M4.5 is the first consumer.

## Acceptance criteria

- [ ] `( cd src-tauri && cargo build )` succeeds with zero warnings.
- [ ] `( cd src-tauri && cargo clippy --all-targets -- -D warnings )` succeeds.
- [ ] `( cd src-tauri && cargo fmt --check )` succeeds.
- [ ] `( cd src-tauri && cargo test parse )` runs the new unit tests; all pass.
- [ ] `./test.sh` succeeds end-to-end (with or without `QUILL_TEST_PG_URL`).
- [ ] `grep -n "sqlparser" src-tauri/Cargo.toml` shows the new dep.
- [ ] `grep -c "pub fn" src-tauri/src/parse/mod.rs` returns exactly `1` — only `analyze_completion` is public.
- [ ] `grep -n "tauri::command" src-tauri/src/commands/mod.rs` includes `analyze_completion`.
- [ ] `grep -n "analyze_completion" src-tauri/src/lib.rs` shows the command registered in `generate_handler!`.
- [ ] `grep -n "analyzeCompletion\|CompletionContext" src/lib/tauri.ts` shows the TS mirror and API method.
- [ ] `grep -n "async fn" src-tauri/src/parse/mod.rs` returns zero matches — the analyzer is sync.
- [ ] `grep -nR "tokio_postgres\|sqlx" src-tauri/src/parse/` returns zero matches — no DB I/O in the parse module.
- [ ] `git diff --stat` touches at most five files: `src-tauri/Cargo.toml`, `src-tauri/src/parse/mod.rs` (new), `src-tauri/src/lib.rs`, `src-tauri/src/commands/mod.rs`, `src/lib/tauri.ts`.
- [ ] `pnpm check` succeeds clean.

## Out of scope

- CTE alias resolution (`WITH cte AS (...) SELECT cte.<TAB>`) — explicitly v1.1 per the M4 brief.
- Subquery-in-FROM scope extraction — v1 punts; subquery aliases yield empty `scope_tables`.
- Schema-qualified function completion (`schema.fn(`) — function-arg completion is v1.1.
- UTF-16 ↔ UTF-8 cursor conversion for non-ASCII buffers — v1.1 if needed.
- Multi-statement scope merging (referring to columns of a table from the previous statement) — never; statements are isolated.
- Ranked / fuzzy completions — happens entirely in the frontend (M4.5); the analyzer just classifies position.
- Caching parsed results — every call is cheap; CodeMirror's `validFor` regex avoids most repeats anyway.
- Frontend wiring — **M4.4** (store + bindings) and **M4.5** (CodeMirror source).
- `;`-detection in the frontend — M3.5's `statement.ts` already does that for *running*; for *completion* this Rust analyzer redoes it. Keeping the two implementations independent is intentional: one drives execution, one drives suggestions; cross-coupling them would inflate scope.
