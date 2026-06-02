#!/usr/bin/env bash
# Run all dependency-security audits in one go.
#
# CI runs `cargo-deny`, `pnpm audit`, and `osv-scanner` on every push
# (see .github/workflows/test.yml). This script is the local equivalent —
# useful before a release, after a dep bump, or any time you want to
# eyeball the supply chain in one shot.
#
# Continues past failures so you see every finding in one run; exits
# non-zero at the end if anything failed.

set -uo pipefail
cd "$(dirname "$0")"

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

bold()   { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
green()  { printf '\033[32m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }
red()    { printf '\033[31m%s\033[0m\n' "$*"; }

declare -a FAILED=()
declare -a MISSING=()

# Check a tool is on PATH; if not, record it and the install hint.
need() {
  local tool="$1" hint="$2"
  if ! command -v "$tool" >/dev/null 2>&1; then
    yellow "skip: $tool not installed → $hint"
    MISSING+=("$tool")
    return 1
  fi
}

# 1. cargo-audit — RustSec advisory DB. Overlaps with cargo-deny's
#    advisories check but uses a different code path and sometimes
#    reports earlier; cheap to run both.
bold "cargo audit (RustSec)"
if need cargo-audit "cargo install --locked cargo-audit"; then
  ( cd src-tauri && cargo audit ) || FAILED+=("cargo-audit")
fi

# 2. cargo-deny — broader policy: advisories + bans + sources
#    (same command as CI, so a local pass means CI will pass too).
bold "cargo deny (advisories + bans + sources)"
if need cargo-deny "cargo install --locked cargo-deny"; then
  ( cd src-tauri && cargo deny check advisories bans sources ) || FAILED+=("cargo-deny")
fi

# 3. pnpm audit — GitHub advisory DB for the JS tree.
#    --audit-level low so you see moderate/low locally (CI gates on high).
bold "pnpm audit (GHSA, --prod, low+)"
if need pnpm "https://pnpm.io/installation"; then
  pnpm audit --prod --audit-level low || FAILED+=("pnpm-audit")
fi

# 4. osv-scanner — cross-ecosystem against Google's OSV DB.
#    Reads Cargo.lock and pnpm-lock.yaml in one pass; catches advisories
#    that the per-ecosystem tools haven't yet picked up.
bold "osv-scanner (Rust + npm via OSV)"
if need osv-scanner \
   "go install github.com/google/osv-scanner/v2/cmd/osv-scanner@latest, or download from https://github.com/google/osv-scanner/releases"; then
  osv-scanner --recursive --skip-git . || FAILED+=("osv-scanner")
fi

# Summary.
bold "Summary"
if [ ${#MISSING[@]} -gt 0 ]; then
  yellow "Not installed (skipped): ${MISSING[*]}"
fi
if [ ${#FAILED[@]} -eq 0 ] && [ ${#MISSING[@]} -eq 0 ]; then
  green "All audits passed."
  exit 0
elif [ ${#FAILED[@]} -eq 0 ]; then
  yellow "No findings, but some auditors were missing — install the above and re-run."
  exit 0
else
  red "Findings in: ${FAILED[*]}"
  exit 1
fi
