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
