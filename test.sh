#!/usr/bin/env bash
# Run all tests.
#   - cargo test         : Rust unit + integration tests (in src-tauri/)
#   - pnpm check         : Svelte/TypeScript type-check across the frontend
# Both must pass for the script to exit 0.
set -euo pipefail
cd "$(dirname "$0")"

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

echo "==> Rust tests…"
( cd src-tauri && cargo test )

echo "==> Frontend type-check…"
pnpm check

echo "==> All checks passed."
