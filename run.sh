#!/usr/bin/env bash
# Start the app in dev mode (hot-reload for both Svelte and Rust).
# First run compiles all Rust deps and takes a few minutes; subsequent runs are fast.
set -euo pipefail
cd "$(dirname "$0")"

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

pnpm tauri dev
