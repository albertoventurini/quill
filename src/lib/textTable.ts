import type { ColumnMeta } from "./tauri";

function field(value: unknown): string {
  let text: string;
  if (value === null) text = "NULL";
  else if (typeof value === "object" && value && !Array.isArray(value) && "__quill_unsupported__" in value)
    text = `«${String(value.__quill_unsupported__)}» (unsupported)`;
  else if (typeof value === "object") text = JSON.stringify(value);
  else text = String(value);
  return text.replace(/\\/g, "\\\\").replace(/\|/g, "\\|").replace(/\t/g, "\\t").replace(/\r/g, "\\r").replace(/\n/g, "\\n");
}

export function encodeTextTable(columns: ColumnMeta[], rows: unknown[][]): string {
  const lines = [columns.map(c => field(c.name)), ...rows.map(r => columns.map((_, i) => field(r[i])))];
  const widths = columns.map((_, i) => lines.reduce((width, r) => Math.max(width, Array.from(r[i]).length), 0));
  const render = (row: string[]) => row.map((s, i) => s + " ".repeat(widths[i] - Array.from(s).length)).join(" | ");
  return [render(lines[0]), widths.map(w => "-".repeat(w)).join("-+-"), ...lines.slice(1).map(render)].join("\n");
}
