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
use sqlparser::tokenizer::{Token, Tokenizer, Word};

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
// Internal token representation with byte offsets
// ---------------------------------------------------------------------------

/// A token annotated with byte offsets into the source buffer.
#[derive(Debug, Clone)]
struct Atok {
    token: Token,
    start: usize,
    end: usize,
}

/// Convert line/column `Location` to a byte offset in `sql`.
/// Uses pre-computed line-start positions for O(1) lookup per token.
fn to_byte_offset(line_starts: &[usize], sql_len: usize, line: u64, col: u64) -> usize {
    if line == 0 || col == 0 {
        return 0;
    }
    let line_idx = (line as usize).saturating_sub(1);
    let line_start = line_starts.get(line_idx).copied().unwrap_or(sql_len);
    (line_start + (col as usize).saturating_sub(1)).min(sql_len)
}

/// Pre-compute the byte offset where each line starts.
fn build_line_starts(sql: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, ch) in sql.char_indices() {
        if ch == '\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Tokenize and annotate every token with byte offsets.
fn tokenize_annotated(sql: &str) -> Result<Vec<Atok>, ()> {
    let dialect = PostgreSqlDialect {};
    let raw = Tokenizer::new(&dialect, sql)
        .tokenize_with_location()
        .map_err(|_| ())?;
    let line_starts = build_line_starts(sql);
    let sql_len = sql.len();
    Ok(raw
        .into_iter()
        .map(|t| {
            let start = to_byte_offset(
                &line_starts,
                sql_len,
                t.span.start.line,
                t.span.start.column,
            );
            let end = to_byte_offset(&line_starts, sql_len, t.span.end.line, t.span.end.column);
            Atok {
                token: t.token,
                start,
                end,
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct PrefixInfo {
    prefix: String,
    from_offset: usize,
    /// Index in `stmt_tokens` of the token whose body contains `from_offset`,
    /// or `None` if `from_offset` is in whitespace.
    prefix_token_idx: Option<usize>,
    /// Index in `stmt_tokens` of the token at or immediately before the
    /// cursor.  This is always `Some` and used as the search-start anchor
    /// for `decide_kind`.
    cursor_anchor_idx: usize,
}

fn cursor_in_comment(sql: &str, cursor: usize) -> bool {
    let cursor = cursor.min(sql.len());
    let bytes = sql.as_bytes();
    let mut i: usize = 0;

    while i < bytes.len() && i <= cursor {
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            let cstart = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if cursor > cstart && cursor <= i {
                return true;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let cstart = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            let cend = if i + 1 < bytes.len() {
                i + 2
            } else {
                bytes.len()
            };
            if cursor > cstart && cursor < cend {
                return true;
            }
            i = cend;
            continue;
        }
        i += 1;
    }
    false
}

fn cursor_in_dollar_quote(sql: &str, cursor: usize) -> bool {
    let cursor = cursor.min(sql.len());
    let bytes = sql.as_bytes();
    let mut i: usize = 0;

    while i < bytes.len() && i <= cursor {
        if bytes[i] == b'$' {
            let dstart = i;
            i += 1;
            // Read optional tag
            let tag_start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'$' {
                let tag = &bytes[tag_start..i];
                i += 1; // skip closing $
                // Look for matching close tag
                while i < bytes.len() {
                    if bytes[i] == b'$' {
                        let _close_start = i;
                        i += 1;
                        let ct_start = i;
                        while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                            i += 1;
                        }
                        if i < bytes.len() && bytes[i] == b'$' {
                            let close_tag = &bytes[ct_start..i];
                            if close_tag == tag {
                                let cend = i + 1;
                                if cursor > dstart && cursor < cend {
                                    return true;
                                }
                                i = cend;
                                break;
                            }
                        }
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
        }
        i += 1;
    }
    false
}

fn cursor_in_string(stmt_tokens: &[Atok], cursor: usize) -> bool {
    for t in stmt_tokens {
        if matches!(
            t.token,
            Token::SingleQuotedString(_)
                | Token::DoubleQuotedString(_)
                | Token::NationalStringLiteral(_)
                | Token::EscapedStringLiteral(_)
                | Token::HexStringLiteral(_)
                | Token::SingleQuotedByteStringLiteral(_)
        ) && cursor > t.start
            && cursor < t.end
        {
            return true;
        }
    }
    false
}

fn find_statement_window(tokens: &[Atok], cursor: usize, sql_len: usize) -> (usize, usize) {
    let mut start = 0usize;
    let mut end = sql_len;
    for t in tokens {
        if matches!(t.token, Token::SemiColon) {
            if t.start < cursor {
                start = t.start + 1;
            } else if t.start >= cursor {
                end = t.start;
                break;
            }
        }
    }
    (start, end)
}

fn in_window(start: usize, end: usize, window_start: usize, window_end: usize) -> bool {
    start >= window_start && end <= window_end
}

fn find_prefix_token(stmt_tokens: &[Atok], sql: &str, cursor: usize) -> PrefixInfo {
    if stmt_tokens.is_empty() {
        return PrefixInfo {
            prefix: String::new(),
            from_offset: cursor,
            prefix_token_idx: None,
            cursor_anchor_idx: 0,
        };
    }

    let mut anchor = 0usize;

    for (idx, t) in stmt_tokens.iter().enumerate() {
        // Cursor strictly inside token body
        if cursor > t.start && cursor < t.end {
            return if matches!(t.token, Token::Word(_)) {
                let slice = &sql[t.start..cursor.min(sql.len())];
                PrefixInfo {
                    prefix: slice.to_string(),
                    from_offset: t.start,
                    prefix_token_idx: Some(idx),
                    cursor_anchor_idx: idx,
                }
            } else {
                PrefixInfo {
                    prefix: String::new(),
                    from_offset: cursor,
                    prefix_token_idx: None,
                    cursor_anchor_idx: idx,
                }
            };
        }
        // Cursor exactly at token end — for Word, treat as at-end-of-word
        if cursor == t.end {
            return if matches!(t.token, Token::Word(_)) {
                let slice = &sql[t.start..t.end];
                PrefixInfo {
                    prefix: slice.to_string(),
                    from_offset: t.start,
                    prefix_token_idx: Some(idx),
                    cursor_anchor_idx: idx,
                }
            } else {
                PrefixInfo {
                    prefix: String::new(),
                    from_offset: cursor,
                    prefix_token_idx: None,
                    cursor_anchor_idx: idx,
                }
            };
        }
        // Cursor at token start — for Word, the cursor is at the first char
        if cursor == t.start && matches!(t.token, Token::Word(_)) {
            return PrefixInfo {
                prefix: String::new(),
                from_offset: t.start,
                prefix_token_idx: Some(idx),
                cursor_anchor_idx: idx,
            };
        }

        // Track the last token that ends before the cursor.
        if t.end <= cursor {
            anchor = idx;
        }

        if t.start > cursor {
            break;
        }
    }

    PrefixInfo {
        prefix: String::new(),
        from_offset: cursor,
        prefix_token_idx: None,
        cursor_anchor_idx: anchor,
    }
}

fn is_clause_keyword(word: &Word) -> bool {
    use sqlparser::keywords::Keyword;
    matches!(
        word.keyword,
        Keyword::SELECT
            | Keyword::FROM
            | Keyword::WHERE
            | Keyword::GROUP
            | Keyword::BY
            | Keyword::ORDER
            | Keyword::HAVING
            | Keyword::JOIN
            | Keyword::INNER
            | Keyword::LEFT
            | Keyword::RIGHT
            | Keyword::FULL
            | Keyword::OUTER
            | Keyword::CROSS
            | Keyword::LATERAL
            | Keyword::ON
            | Keyword::USING
            | Keyword::LIMIT
            | Keyword::OFFSET
            | Keyword::UNION
            | Keyword::INTERSECT
            | Keyword::EXCEPT
    )
}

fn is_from_keyword(word: &Word) -> bool {
    use sqlparser::keywords::Keyword;
    matches!(
        word.keyword,
        Keyword::FROM
            | Keyword::JOIN
            | Keyword::INNER
            | Keyword::LEFT
            | Keyword::RIGHT
            | Keyword::FULL
            | Keyword::OUTER
            | Keyword::CROSS
            | Keyword::LATERAL
    )
}

fn is_stop_keyword(word: &Word) -> bool {
    use sqlparser::keywords::Keyword;
    matches!(
        word.keyword,
        Keyword::WHERE
            | Keyword::GROUP
            | Keyword::ORDER
            | Keyword::HAVING
            | Keyword::LIMIT
            | Keyword::OFFSET
            | Keyword::UNION
            | Keyword::INTERSECT
            | Keyword::EXCEPT
            | Keyword::SELECT
    )
}

fn is_join_kw(word: &Word) -> bool {
    use sqlparser::keywords::Keyword;
    matches!(
        word.keyword,
        Keyword::JOIN
            | Keyword::INNER
            | Keyword::LEFT
            | Keyword::RIGHT
            | Keyword::FULL
            | Keyword::OUTER
            | Keyword::CROSS
            | Keyword::LATERAL
    )
}

fn decide_kind(stmt_tokens: &[Atok], prefix: &PrefixInfo) -> (CompletionKind, Option<String>) {
    if stmt_tokens.is_empty() {
        return (CompletionKind::Unqualified, None);
    }

    // Start search from the anchor (the last token before or at cursor).
    // Walk backwards from the anchor, collecting the two most recent
    // non-whitespace tokens that aren't the prefix token itself.
    let search_start = prefix
        .cursor_anchor_idx
        .min(stmt_tokens.len().saturating_sub(1)) as isize;

    let mut prev_non_ws: Option<(&Atok, isize)> = None;
    let mut prev_prev_non_ws: Option<&Atok> = None;

    let mut i: isize = search_start;
    while i >= 0 {
        let t = &stmt_tokens[i as usize];
        if !matches!(t.token, Token::Whitespace(_)) && Some(i as usize) != prefix.prefix_token_idx {
            if prev_non_ws.is_none() {
                prev_non_ws = Some((t, i));
            } else if prev_prev_non_ws.is_none() {
                prev_prev_non_ws = Some(t);
                break;
            }
        }
        i -= 1;
    }

    let (prev, prev_idx) = match prev_non_ws {
        Some(p) => p,
        None => return (CompletionKind::Unqualified, None),
    };

    match &prev.token {
        Token::Period => {
            if let Some(pp) = prev_prev_non_ws
                && let Token::Word(w) = &pp.token
            {
                // Reject clause keywords as qualifiers (SELECT.public is
                // not meaningful), but accept any other identifier
                // including regular PostgreSQL keywords like PUBLIC
                // which are commonly used as schema names.
                if is_clause_keyword(w) && w.quote_style.is_none() {
                    return (CompletionKind::Unqualified, None);
                }
                let qualifier = w.value.clone();
                let from_depth = paren_depth_at(stmt_tokens, prev_idx as usize);
                let kind = if is_token_in_from_clause(stmt_tokens, prev_idx as usize, from_depth) {
                    CompletionKind::QualifiedRelation
                } else {
                    CompletionKind::QualifiedColumn
                };
                return (kind, Some(qualifier));
            }
            (CompletionKind::Unqualified, None)
        }

        Token::Word(w) if is_from_keyword(w) => (CompletionKind::FromItem, None),

        Token::Word(w) if is_clause_keyword(w) => (CompletionKind::Unqualified, None),

        Token::Comma => {
            let from_depth = paren_depth_at(stmt_tokens, prev_idx as usize);
            if is_token_in_from_clause(stmt_tokens, prev_idx as usize, from_depth) {
                (CompletionKind::FromItem, None)
            } else {
                (CompletionKind::Unqualified, None)
            }
        }

        _ => (CompletionKind::Unqualified, None),
    }
}

fn paren_depth_at(stmt_tokens: &[Atok], idx: usize) -> i32 {
    let mut depth: i32 = 0;
    let mut i = idx as isize;
    while i >= 0 {
        match stmt_tokens[i as usize].token {
            Token::LParen => depth -= 1,
            Token::RParen => depth += 1,
            _ => {}
        }
        i -= 1;
    }
    depth
}

fn is_token_in_from_clause(stmt_tokens: &[Atok], idx: usize, _depth: i32) -> bool {
    let mut i = idx as isize;
    let mut parens: i32 = 0;
    while i >= 0 {
        let t = &stmt_tokens[i as usize];
        match &t.token {
            Token::RParen => parens += 1,
            Token::LParen => parens -= 1,
            Token::Word(w) if parens == 0 => {
                if is_from_keyword(w) {
                    return true;
                }
                // ON / USING open a join *condition* — a boolean expression
                // over the joined tables. Hitting one before any FROM/JOIN
                // keyword means the qualifier is a column reference (alias.col),
                // not a schema-qualified relation.
                if matches!(
                    w.keyword,
                    sqlparser::keywords::Keyword::WHERE
                        | sqlparser::keywords::Keyword::SELECT
                        | sqlparser::keywords::Keyword::GROUP
                        | sqlparser::keywords::Keyword::ORDER
                        | sqlparser::keywords::Keyword::HAVING
                        | sqlparser::keywords::Keyword::LIMIT
                        | sqlparser::keywords::Keyword::OFFSET
                        | sqlparser::keywords::Keyword::ON
                        | sqlparser::keywords::Keyword::USING
                ) {
                    return false;
                }
            }
            _ => {}
        }
        i -= 1;
    }
    false
}

fn extract_scope_tables(stmt_tokens: &[Atok], cursor: usize) -> Vec<ScopeTable> {
    // Compute cursor paren depth (depth of open parens at cursor position).
    let mut cursor_depth: i32 = 0;
    for t in stmt_tokens.iter() {
        if t.start >= cursor {
            break;
        }
        match &t.token {
            Token::LParen => cursor_depth += 1,
            Token::RParen if cursor_depth > 0 => {
                cursor_depth -= 1;
            }
            _ => {}
        }
    }

    // Find the SELECT that governs the cursor: the last top-level (at
    // cursor_depth) SELECT keyword that starts at or before the cursor. The
    // buffer may hold several SELECTs with no semicolon between them (which
    // would otherwise share one statement window), so anchoring to the
    // governing SELECT keeps scope extraction confined to the cursor's query.
    let mut select_idx: Option<usize> = None;
    let mut depth: i32 = 0;
    for (i, t) in stmt_tokens.iter().enumerate() {
        match &t.token {
            Token::LParen => depth += 1,
            Token::RParen if depth > 0 => {
                depth -= 1;
            }
            Token::Word(w)
                if depth == cursor_depth
                    && w.keyword == sqlparser::keywords::Keyword::SELECT
                    && t.start <= cursor =>
            {
                select_idx = Some(i);
            }
            _ => {}
        }
    }

    // Find the FROM clause belonging to the governing SELECT. Scan past the
    // cursor (the FROM may sit after it in SELECT-list contexts), but stop at
    // the next SELECT at cursor_depth: hitting it first means this query has no
    // FROM, and we must not borrow the following query's FROM.
    let mut from_idx: Option<usize> = None;
    let mut depth: i32 = 0;
    for (i, t) in stmt_tokens.iter().enumerate() {
        match &t.token {
            Token::LParen => depth += 1,
            Token::RParen if depth > 0 => {
                depth -= 1;
            }
            Token::Word(w) if depth == cursor_depth => {
                let after_governing = select_idx.is_none_or(|s| i > s);
                if !after_governing {
                    continue;
                }
                if w.keyword == sqlparser::keywords::Keyword::SELECT {
                    break;
                }
                if is_from_keyword(w) {
                    from_idx = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }

    let from_idx = match from_idx {
        Some(i) => i,
        None => return Vec::new(),
    };

    walk_table_list(stmt_tokens, from_idx, cursor_depth)
}

fn walk_table_list(tokens: &[Atok], mut i: usize, max_depth: i32) -> Vec<ScopeTable> {
    let mut tables = Vec::new();
    let mut depth: i32 = 0;
    let mut seen_from = false;
    let len = tokens.len();
    let base_depth = max_depth;

    loop {
        if i >= len {
            break;
        }
        let t = &tokens[i];

        match &t.token {
            Token::LParen => {
                depth += 1;
                let saved = i;
                if let Some(sc) = walk_subquery(tokens, &mut i, len) {
                    tables.push(sc);
                }
                if i == saved {
                    i += 1;
                }
                continue;
            }

            Token::RParen => {
                depth -= 1;
                i += 1;
                continue;
            }

            Token::Word(w) => {
                use sqlparser::keywords::Keyword;
                if !seen_from && is_from_keyword(w) {
                    seen_from = true;
                    i += 1;
                    continue;
                }
                if depth <= base_depth && is_stop_keyword(w) {
                    break;
                }
                if depth <= base_depth && is_join_kw(w) {
                    i += 1;
                    continue;
                }

                if matches!(w.keyword, Keyword::ON | Keyword::USING) {
                    skip_join_condition(tokens, &mut i, len, base_depth);
                    continue;
                }

                if matches!(w.keyword, Keyword::AS) {
                    i += 1;
                    continue;
                }

                // Skip other clause keywords.
                if depth <= base_depth && is_clause_keyword(w) {
                    i += 1;
                    continue;
                }

                // Any word reaching here sits in table-name position: every
                // structural keyword (FROM/JOIN/ON/USING/AS, plus stop and
                // clause keywords) was consumed by the branches above. Postgres
                // lets non-reserved keywords (`action`, `name`, `type`,
                // `value`, …) be unquoted table names, so accept them too.
                // Restricting to `NoKeyword` dropped such tables and misread
                // the following alias as the relation name.
                let (schema, name, alias) = read_relation(tokens, &mut i, len, base_depth);
                if let Some(name) = name {
                    tables.push(ScopeTable {
                        schema,
                        name,
                        alias,
                    });
                }
                continue;
            }

            Token::Comma => {
                i += 1;
                continue;
            }

            _ => {
                i += 1;
            }
        }
    }

    tables
}

fn skip_join_condition(tokens: &[Atok], i: &mut usize, len: usize, base_depth: i32) {
    let t = &tokens[*i];
    match &t.token {
        Token::Word(w) if w.keyword == sqlparser::keywords::Keyword::USING => {
            *i += 1;
            let mut parens: i32 = 1;
            while *i < len && parens > 0 {
                match &tokens[*i].token {
                    Token::LParen => parens += 1,
                    Token::RParen => parens -= 1,
                    _ => {}
                }
                *i += 1;
            }
        }
        _ => {
            *i += 1;
            let mut depth: i32 = 0;
            while *i < len {
                let tok = &tokens[*i].token;
                match tok {
                    Token::LParen => depth += 1,
                    Token::RParen => {
                        depth -= 1;
                        if depth < 0 {
                            break;
                        }
                    }
                    Token::Word(w)
                        if depth <= base_depth && (is_join_kw(w) || is_stop_keyword(w)) =>
                    {
                        break;
                    }
                    Token::Comma if depth <= base_depth => break,
                    _ => {}
                }
                *i += 1;
            }
        }
    }
}

fn walk_subquery(tokens: &[Atok], i: &mut usize, len: usize) -> Option<ScopeTable> {
    let mut depth: i32 = 1;
    *i += 1;
    while *i < len && depth > 0 {
        match &tokens[*i].token {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            _ => {}
        }
        if depth > 0 {
            *i += 1;
        }
    }
    if *i >= len {
        return None;
    }
    *i += 1; // skip past closing paren
    // Consume trailing alias (if any)
    read_alias(tokens, i, len);
    None // v1: don't extract aliases from subqueries
}

fn read_relation(
    tokens: &[Atok],
    i: &mut usize,
    len: usize,
    _base_depth: i32,
) -> (Option<String>, Option<String>, Option<String>) {
    let first = match &tokens[*i].token {
        Token::Word(w) => Some(w.value.clone()),
        _ => {
            *i += 1;
            return (None, None, None);
        }
    };
    *i += 1;
    skip_ws(tokens, i, len);

    // Check for schema.name
    if *i < len && matches!(&tokens[*i].token, Token::Period) {
        *i += 1;
        skip_ws(tokens, i, len);
        if *i < len
            && let Token::Word(w) = &tokens[*i].token
        {
            let name = w.value.clone();
            *i += 1;
            let alias = read_alias(tokens, i, len);
            return (first, Some(name), alias);
        }
    }

    let alias = read_alias(tokens, i, len);
    (None, first, alias)
}

fn read_alias(tokens: &[Atok], i: &mut usize, len: usize) -> Option<String> {
    use sqlparser::keywords::Keyword;

    skip_ws(tokens, i, len);

    if *i >= len {
        return None;
    }

    let has_as = matches!(&tokens[*i].token, Token::Word(w) if w.keyword == Keyword::AS);
    if has_as {
        *i += 1;
        skip_ws(tokens, i, len);
    }

    if *i >= len {
        return None;
    }

    if let Token::Word(w) = &tokens[*i].token {
        if is_clause_keyword(w) {
            return None;
        }
        let alias = w.value.clone();
        *i += 1;
        return Some(alias);
    }

    None
}

fn skip_ws(tokens: &[Atok], i: &mut usize, len: usize) {
    while *i < len && matches!(tokens[*i].token, Token::Whitespace(_)) {
        *i += 1;
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Analyze the SQL buffer at the given byte-offset cursor and return a
/// [`CompletionContext`] describing what the frontend should suggest.
pub fn analyze_completion(sql: &str, cursor: usize) -> CompletionContext {
    let cursor = cursor.min(sql.len());

    // 1. Tokenize the whole buffer once.
    let tokens = match tokenize_annotated(sql) {
        Ok(t) => t,
        Err(()) => return CompletionContext::none(cursor),
    };

    // 2. Check whether the cursor is in a comment or dollar-quoted string.
    if cursor_in_comment(sql, cursor) || cursor_in_dollar_quote(sql, cursor) {
        return CompletionContext::none(cursor);
    }

    // 3. Find the cursor's statement window.
    let (stmt_start, stmt_end) = find_statement_window(&tokens, cursor, sql.len());

    // 4. Filter tokens to the statement window.
    let stmt_tokens: Vec<Atok> = tokens
        .iter()
        .filter(|t| in_window(t.start, t.end, stmt_start, stmt_end))
        .cloned()
        .collect();

    // 5. Find the prefix-bearing token.
    let prefix_info = find_prefix_token(&stmt_tokens, sql, cursor);

    // 6. Detect whether cursor sits inside an open string literal.
    if cursor_in_string(&stmt_tokens, cursor) {
        return CompletionContext::none(prefix_info.from_offset);
    }

    // 7. Decide the kind + qualifier.
    let (kind, qualifier) = decide_kind(&stmt_tokens, &prefix_info);

    // 8. Extract scope_tables.
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
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(sql: &str) -> CompletionContext {
        let cursor = sql
            .find('|')
            .expect("test SQL must contain a `|` cursor marker");
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
            vec![ScopeTable {
                schema: None,
                name: "users".into(),
                alias: Some("u".into())
            }],
        );
    }

    #[test]
    fn unqualified_in_select_list_with_scope() {
        let c = ctx("SELECT em| FROM users");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert_eq!(c.prefix, "em");
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable {
                schema: None,
                name: "users".into(),
                alias: None
            }],
        );
    }

    #[test]
    fn scope_includes_join_targets_with_aliases() {
        let c = ctx("SELECT | FROM users u JOIN orders AS o ON u.id = o.user_id");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert_eq!(
            c.scope_tables,
            vec![
                ScopeTable {
                    schema: None,
                    name: "users".into(),
                    alias: Some("u".into())
                },
                ScopeTable {
                    schema: None,
                    name: "orders".into(),
                    alias: Some("o".into())
                },
            ],
        );
    }

    #[test]
    fn schema_qualified_table_in_from_keeps_schema() {
        let c = ctx("SELECT | FROM common.events e");
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable {
                schema: Some("common".into()),
                name: "events".into(),
                alias: Some("e".into())
            }],
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
            vec![ScopeTable {
                schema: None,
                name: "orders".into(),
                alias: None
            }],
        );
    }

    #[test]
    fn subquery_in_from_does_not_add_to_scope_in_v1() {
        let c = ctx("SELECT | FROM (SELECT id FROM users) u");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert!(c.scope_tables.is_empty());
    }

    #[test]
    fn unqualified_in_where_with_scope() {
        let c = ctx("SELECT * FROM users WHERE |");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable {
                schema: None,
                name: "users".into(),
                alias: None
            }],
        );
    }

    #[test]
    fn cursor_at_end_of_word_includes_full_word_as_prefix() {
        let c = ctx("SELECT users|");
        assert_eq!(c.prefix, "users");
        assert_eq!(c.from_offset, "SELECT ".len());
    }

    #[test]
    fn period_with_no_qualifier_is_unqualified() {
        let c = ctx("SELECT .|");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert_eq!(c.prefix, "");
    }

    #[test]
    fn qualified_with_space_after_period() {
        let c = ctx("SELECT u. | FROM users u");
        assert_eq!(c.kind, CompletionKind::QualifiedColumn);
        assert_eq!(c.qualifier.as_deref(), Some("u"));
        assert_eq!(c.prefix, "");
    }

    #[test]
    fn comma_in_from_list_is_from_item() {
        let c = ctx("SELECT * FROM users, |");
        assert_eq!(c.kind, CompletionKind::FromItem);
        assert_eq!(c.prefix, "");
    }

    #[test]
    fn keyword_named_table_is_read_with_its_alias() {
        // Regression: `action` is Keyword::ACTION in sqlparser. Restricting
        // scope extraction to NoKeyword words dropped the table and misread
        // its alias `a` as the relation name, so `a.` resolved to nothing.
        let c = ctx("SELECT * FROM action a LEFT JOIN action_ban_actor aba ON aba.id = a.|");
        assert_eq!(c.kind, CompletionKind::QualifiedColumn);
        assert_eq!(c.qualifier.as_deref(), Some("a"));
        assert_eq!(
            c.scope_tables,
            vec![
                ScopeTable {
                    schema: None,
                    name: "action".into(),
                    alias: Some("a".into())
                },
                ScopeTable {
                    schema: None,
                    name: "action_ban_actor".into(),
                    alias: Some("aba".into())
                },
            ],
        );
    }

    #[test]
    fn keyword_named_table_without_alias_keeps_its_name() {
        let c = ctx("SELECT | FROM action");
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable {
                schema: None,
                name: "action".into(),
                alias: None
            }],
        );
    }

    #[test]
    fn alias_qualifier_in_on_clause_is_qualified_column() {
        // Regression: a `ts.` inside an ON condition must resolve as a
        // column qualifier, not a schema-qualified relation. Walking back
        // from `ts` hits JOIN before any stop keyword, so without treating
        // ON as a boundary this was misclassified as QualifiedRelation.
        let c = ctx("SELECT * FROM \"schema_a\".\"trust_status\" ts \
             LEFT JOIN tiko_connector tc ON ts.|uuid = tc.uuid");
        assert_eq!(c.kind, CompletionKind::QualifiedColumn);
        assert_eq!(c.qualifier.as_deref(), Some("ts"));
        assert_eq!(
            c.scope_tables,
            vec![
                ScopeTable {
                    schema: Some("schema_a".into()),
                    name: "trust_status".into(),
                    alias: Some("ts".into())
                },
                ScopeTable {
                    schema: None,
                    name: "tiko_connector".into(),
                    alias: Some("tc".into())
                },
            ],
        );
    }

    #[test]
    fn second_alias_qualifier_in_on_clause_is_qualified_column() {
        let c = ctx("SELECT * FROM users u JOIN orders o ON u.id = o.|");
        assert_eq!(c.kind, CompletionKind::QualifiedColumn);
        assert_eq!(c.qualifier.as_deref(), Some("o"));
    }

    #[test]
    fn schema_qualified_relation_after_on_clause_still_qualified_relation() {
        // A genuine schema-qualified relation in a *subsequent* JOIN target
        // must remain QualifiedRelation: walking back hits JOIN before ON.
        let c = ctx("SELECT * FROM a JOIN b ON a.x = b.y JOIN common.|");
        assert_eq!(c.kind, CompletionKind::QualifiedRelation);
        assert_eq!(c.qualifier.as_deref(), Some("common"));
    }

    #[test]
    fn cursor_before_first_token_is_unqualified() {
        let c = ctx("|SELECT 1");
        assert_eq!(c.kind, CompletionKind::Unqualified);
        assert_eq!(c.prefix, "");
        assert!(c.scope_tables.is_empty());
    }

    #[test]
    fn scope_ignores_prior_unterminated_select() {
        // No semicolon between the two SELECTs: both live in one statement
        // window. Scope must come from the cursor's query, not the first FROM.
        let c = ctx(
            "SELECT * FROM users u\n\
             SELECT * from trust_status ts \
             LEFT JOIN tiko_connector tc on ts.uuid = tc.|",
        );
        assert_eq!(c.kind, CompletionKind::QualifiedColumn);
        assert_eq!(c.qualifier.as_deref(), Some("tc"));
        assert_eq!(
            c.scope_tables,
            vec![
                ScopeTable {
                    schema: None,
                    name: "trust_status".into(),
                    alias: Some("ts".into())
                },
                ScopeTable {
                    schema: None,
                    name: "tiko_connector".into(),
                    alias: Some("tc".into())
                },
            ],
        );
    }

    #[test]
    fn scope_for_first_of_two_unterminated_selects() {
        let c = ctx("SELECT a| FROM t1\nSELECT b FROM t2");
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable {
                schema: None,
                name: "t1".into(),
                alias: None
            }],
        );
    }

    #[test]
    fn single_select_scope_unchanged() {
        let c = ctx("SELECT col| FROM orders o WHERE o.id = 1");
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable {
                schema: None,
                name: "orders".into(),
                alias: Some("o".into())
            }],
        );
    }

    #[test]
    fn union_second_branch_scope() {
        let c = ctx("SELECT a FROM t1 UNION SELECT b| FROM t2");
        assert_eq!(
            c.scope_tables,
            vec![ScopeTable {
                schema: None,
                name: "t2".into(),
                alias: None
            }],
        );
    }
}
