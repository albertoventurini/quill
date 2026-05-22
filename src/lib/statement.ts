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
