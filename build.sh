#!/usr/bin/env bash
# Produce an optimized release build.
# Outputs:
#   src-tauri/target/release/quill                  — the binary
#   src-tauri/target/release/bundle/                — platform installer (.deb / .AppImage on Linux)
# Much slower than `run.sh` because it fully optimizes Rust + bundles the frontend.
set -euo pipefail
cd "$(dirname "$0")"

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

pnpm tauri build
