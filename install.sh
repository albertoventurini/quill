#!/usr/bin/env bash
# Install/update all dependencies (frontend + Rust).
# Run after cloning the repo, or whenever you add deps to package.json or Cargo.toml.
set -euo pipefail
cd "$(dirname "$0")"

# Make cargo/rustc available even if this shell didn't load ~/.cargo/env yet.
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

echo "==> Installing frontend deps (pnpm)…"
pnpm install

# pnpm 11+ blocks post-install scripts for packages like esbuild until they're
# explicitly approved. --all says: yes, run them all. Approvals are recorded in
# pnpm-workspace.yaml so this is a no-op after the first run.
echo "==> Approving any pending native-build scripts…"
pnpm approve-builds --all || true

echo "==> Fetching Rust crates (cargo)…"
( cd src-tauri && cargo fetch )

echo "==> Done."
