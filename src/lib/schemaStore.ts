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
