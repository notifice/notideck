#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Set the Notideck version everywhere it lives.
#   scripts/set-version.sh X.Y.Z
# Updates host/Cargo.toml ([workspace.package] version), any crate that hardcodes its own
# version, Cargo.lock (workspace entries only) and android/app/build.gradle
# (versionName "X.Y.Z", versionCode X*10000+Y*100+Z).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
case "${1:-}" in -h|--help|"") sed -n '2,6p' "$0"; [ -n "${1:-}" ] && exit 0 || exit 2 ;; esac
VER="$1"
if ! [[ "$VER" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "error: '$VER' is not a plain semver X.Y.Z" >&2; exit 2
fi
MA="${BASH_REMATCH[1]}"; MI="${BASH_REMATCH[2]}"; PA="${BASH_REMATCH[3]}"
if [ "$MI" -gt 99 ] || [ "$PA" -gt 99 ]; then
  echo "error: minor and patch must be <= 99 (versionCode = X*10000+Y*100+Z)" >&2; exit 2
fi
CODE=$((MA * 10000 + MI * 100 + PA))

CARGO="$REPO/host/Cargo.toml"
GRADLE="$REPO/android/app/build.gradle"
OLD="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$CARGO" | head -1)"
[ -n "$OLD" ] || { echo "error: no [workspace.package] version in $CARGO" >&2; exit 1; }

# Workspace version.
sed -i '/^\[workspace.package\]/,/^\[/{s/^version *= *".*"/version = "'"$VER"'"/}' "$CARGO"
echo "host/Cargo.toml: workspace version $OLD -> $VER"

# Crates that hardcode a version instead of version.workspace = true.
for f in "$REPO"/host/crates/*/Cargo.toml "$REPO"/host/plugins/*/Cargo.toml; do
  if sed -n '/^\[package\]/,/^\[/p' "$f" | grep -qE '^version *= *"'; then
    sed -i '/^\[package\]/,/^\[/{s/^version *= *".*"/version = "'"$VER"'"/}' "$f"
    echo "${f#"$REPO"/}: hardcoded version -> $VER"
  fi
done

# Cargo.lock: refresh the workspace members only (no dependency changes).
(cd "$REPO/host" && cargo update --workspace --offline 2>&1 | sed 's/^/cargo: /') || {
  echo "error: could not refresh Cargo.lock" >&2; exit 1; }
echo "host/Cargo.lock refreshed"

# Android.
sed -i -E 's/^( *)versionCode [0-9]+/\1versionCode '"$CODE"'/; s/^( *)versionName "[^"]*"/\1versionName "'"$VER"'"/' "$GRADLE"
echo "android/app/build.gradle: versionName \"$VER\", versionCode $CODE"
