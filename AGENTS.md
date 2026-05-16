# Quill — AGENTS.md

Personal-use desktop SQL client for PostgreSQL. Rust + Tauri 2 backend, Svelte 5 + TypeScript frontend. Full design in `PRD.md`.

## Setup, run, test, build
```bash
./install.sh   # install/update deps (pnpm + cargo)
./run.sh       # dev mode, hot reload
./test.sh      # cargo test + svelte-check
./build.sh     # release binary + .deb / .AppImage
```

Always prefer these scripts over invoking `pnpm`/`cargo` directly — they handle sourcing `~/.cargo/env` and the pnpm build-approval gate.

## Layout
- `src-tauri/` — Rust backend. All DB I/O lives here; the frontend never talks to Postgres directly.
- `src/` — Svelte 5 + SvelteKit frontend (adapter-static; produces plain files for the Tauri webview).
- `static/` — public assets.
- Local SQLite (in the platform app-data dir) stores: connections, schema cache, query history, saved queries. **Not** the user's Postgres data.

## Design principles (do not violate without discussion)
1. **No hidden DB connections.** Every active Postgres connection is the result of an explicit user action: connect, expand a database, run a query, refresh schema. No keepalives, no autocomplete fetches, no test-on-borrow.
2. **Pool is a budget, not a default.** Per-server slot count is user-configurable (default 2). Never silently exceed it. This is the whole reason Quill exists: the user's org caps active connections, and DBeaver-style clients trip the limit.
3. **Caching over re-fetching.** Schema introspection runs once on first expand and is cached locally. Refresh is a manual action.
4. **Cancellation is first-class.** Use Postgres's out-of-band `CancelRequest` mechanism; it does not consume a slot.
5. **Async core, synchronous-feeling UI.** All DB work runs in Rust; the UI shows clear busy states rather than spinning while the DB blocks.

## Tech stack
- **Backend:** Rust edition 2024, Tauri 2.x, `sqlx` (postgres feature, runtime-tokio), `sqlparser-rs`, `keyring` (OS keychain), `rusqlite` or `sqlx-sqlite` for the local app store.
- **Frontend:** Svelte 5 (runes), SvelteKit with `adapter-static`, TypeScript, Vite, CodeMirror 6 (`@codemirror/lang-sql`).
- **Tooling:** pnpm 11+. Native build scripts (e.g. `esbuild`) must be approved with `pnpm approve-builds <pkg>`. The legacy `pnpm.onlyBuiltDependencies` field in `package.json` is ignored by pnpm 11 — do not use it. Approvals end up in `pnpm-workspace.yaml`.

## Code style
- Rust: `cargo fmt` + `cargo clippy -- -D warnings`. Idiomatic async/await, `?` for error propagation, `thiserror` for error enums.
- Svelte/TS: Svelte 5 runes (`$state`, `$derived`, `$effect`), TypeScript throughout, default Prettier formatting.
- Comments: only when the *why* is non-obvious (hidden invariant, surprising workaround). Don't narrate what well-named code already says.

## Scope and caution
- Personal project — don't over-engineer. Pick the simplest thing that honors the design principles.
- Postgres-only in v1; don't introduce abstractions for other engines.
- Result grid is read-only in v1; data editing is out of scope.
- Before running any scaffolder / `create-*` / `init` tool with a `--force` or similar flag in a non-empty directory, move important user files (e.g. `PRD.md`) aside first. Those flags can wipe the directory.

## References
- `PRD.md` — full product requirements
- https://tauri.app/ — Tauri 2 docs
- https://svelte.dev/docs — Svelte 5 docs
- https://agents.md/ — convention this file follows
