# Quill

A small PostgreSQL desktop client that respects strict connection caps.

Built because mainstream clients (DBeaver and friends) silently open many background connections — keepalives, schema introspection, autocomplete fetches, test-on-borrow probes — and trip per-user connection limits some organizations enforce. Quill exposes a small, user-configurable connection budget per server (default **2**) and never opens a connection without an explicit user action.

**Status:** v1 in development. See [`PRD.md`](PRD.md) for the full design and [`tasks/`](tasks/) for the work plan.

## Prerequisites

- Rust (install via [rustup](https://rustup.rs/))
- Node 20+ with **pnpm 11+** (`corepack enable && corepack prepare pnpm@latest --activate`)
- On Linux, the GTK/WebKit dev libraries Tauri needs — see [Tauri prerequisites](https://tauri.app/start/prerequisites/)
- A reachable PostgreSQL during development (the easiest is Docker: `docker run -d -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:17`)

## Quickstart

```bash
./install.sh   # install pnpm + cargo deps, approve native build scripts
./run.sh       # dev mode; first run compiles Rust and takes a few minutes
```

## Scripts

| Script | What it does |
|---|---|
| `./install.sh` | Install/update frontend (`pnpm install`) and Rust (`cargo fetch`) deps; auto-approve native build scripts |
| `./run.sh` | `pnpm tauri dev` — hot-reload for both Svelte and Rust |
| `./test.sh` | `cargo test` + `pnpm check` (Svelte/TS type-check) |
| `./build.sh` | `pnpm tauri build` — optimized binary + `.deb` / `.AppImage` |

All scripts source `~/.cargo/env` so they work in any shell.

## Layout

```
.
├── PRD.md            full v1 design spec
├── AGENTS.md         project guide for coding agents
├── CLAUDE.md         symlink → AGENTS.md
├── tasks/            self-contained per-task specs (M1.2–M1.6)
├── src/              Svelte 5 + SvelteKit frontend
├── src-tauri/        Rust backend (all DB I/O lives here)
└── static/           public assets
```

The Rust backend owns every database interaction; the frontend talks to it through typed Tauri commands.

## Design principles

1. **No hidden DB connections** — every active Postgres connection comes from an explicit user action.
2. **Pool is a budget, not a default** — per-server slot count is user-configurable; Quill never silently exceeds it.
3. **Caching over re-fetching** — schema introspection runs once on first expand and is cached; refresh is manual.
4. **Cancellation is first-class** — uses Postgres's out-of-band `CancelRequest`, which doesn't consume a slot.
5. **Async core, synchronous-feeling UI** — all DB work runs in Rust; the UI shows clear busy states.

See `AGENTS.md` for the full project conventions and `PRD.md` for design rationale.

## Tech stack

- **Backend:** Rust edition 2024, Tauri 2.x, `sqlx` (Postgres + SQLite), `sqlparser-rs`
- **Frontend:** Svelte 5 (runes), SvelteKit `adapter-static`, TypeScript, Vite, CodeMirror 6 (editor — planned)
- **Toolchain:** pnpm 11+, cargo, Docker (for a local Postgres in dev)

## License

MIT.
