//! Stable, auto-assigned accent colour per server connection.
//!
//! Used by the tab strip (and reusable in the tree) so a server is
//! identifiable at a glance even when its name is truncated.  Derived from
//! the connection id, which is stable across sessions, so a server keeps the
//! same colour.  No configuration: the first 8 servers get distinct hues.

const PALETTE = [
  "#3b82f6", // blue
  "#10b981", // emerald
  "#f59e0b", // amber
  "#8b5cf6", // violet
  "#ef4444", // red
  "#06b6d4", // cyan
  "#ec4899", // pink
  "#84cc16", // lime
] as const;

export function serverColor(serverId: number): string {
  const n = PALETTE.length;
  return PALETTE[((serverId % n) + n) % n];
}
