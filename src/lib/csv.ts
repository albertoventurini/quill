//! RFC 4180 CSV encoding for the result grid's export and clipboard
//! flows.
//!
//! - Header row from column names (always emitted).
//! - Field quoting when the field contains `,`, `"`, `\r`, or `\n`.
//! - Interior `"` doubled to `""`.
//! - Lines separated by `\r\n` per RFC 4180.
//! - Null and undefined render as empty cells.
//! - Non-string values stringify via `String(...)`; objects via
//!   `JSON.stringify(...)` (compact, no indent).

import type { ColumnMeta } from "./tauri";

const QUOTE_RE = /[,"\r\n]/;

function field(value: unknown): string {
  if (value === null || value === undefined) return "";
  let s: string;
  if (typeof value === "string") s = value;
  else if (typeof value === "object") {
    if (!Array.isArray(value) && "__quill_unsupported__" in value) {
      s = `«${String((value as Record<string, unknown>).__quill_unsupported__)}» (unsupported)`;
    } else {
      try {
        s = JSON.stringify(value);
      } catch {
        s = String(value);
      }
    }
  } else {
    s = String(value);
  }
  if (QUOTE_RE.test(s)) {
    return `"${s.replace(/"/g, '""')}"`;
  }
  return s;
}

/** Encode a result set as RFC 4180 CSV.  `prelude` (if provided) is
 *  prepended verbatim followed by `\r\n` — used for the `.partial`
 *  comment line.  Most consumers will treat that line as a stray row;
 *  the filename `.partial` suffix is the canonical signal. */
export function encodeCsv(
  columns: ColumnMeta[],
  rows: unknown[][],
  prelude?: string,
): string {
  const out: string[] = [];
  if (prelude) out.push(prelude);
  out.push(columns.map((c) => field(c.name)).join(","));
  for (const row of rows) {
    out.push(row.map(field).join(","));
  }
  return out.join("\r\n") + "\r\n";
}
