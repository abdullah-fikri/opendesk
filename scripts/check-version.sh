#!/usr/bin/env bash
# Fails when a component's version differs from VERSION (the single source of truth).
# Usage: scripts/check-version.sh [expected]   (default: contents of VERSION)
set -euo pipefail
cd "$(dirname "$0")/.."

expected="${1:-$(tr -d '[:space:]' < VERSION)}"
status=0

check() {
  local file="$1" actual="$2"
  if [[ "$actual" != "$expected" ]]; then
    echo "::error file=$file::version is '$actual', expected '$expected'"
    status=1
  else
    echo "ok  $file ($actual)"
  fi
}

check VERSION "$(tr -d '[:space:]' < VERSION)"
check client-desktop/Cargo.toml "$(sed -n 's/^version = "\(.*\)"/\1/p' client-desktop/Cargo.toml | head -1)"
check core-engine/Cargo.toml "$(sed -n 's/^version = "\(.*\)"/\1/p' core-engine/Cargo.toml | head -1)"
check server-relay/internal/version/version.go "$(sed -n 's/^var Version = "\(.*\)"/\1/p' server-relay/internal/version/version.go)"
check installer/opendesk.iss "$(sed -n 's/^ *#define MyAppVersion "\(.*\)"/\1/p' installer/opendesk.iss | tr -d '\r')"
# Android reads VERSION directly (app/build.gradle), so it cannot drift.

exit "$status"
