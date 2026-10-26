#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the bundled plugins and lay them out as <out_dir>/<id>/{manifest.json, <exe>, icons/}.
#   scripts/stage-plugins.sh [--target <triple>] [--release] [--only <name,...>] <out_dir>
# Default: the plugins shipped with the app (clock, http, weather). --only picks a subset.
# Development: scripts/stage-plugins.sh host/target/plugins-staged  (the agent's dev fallback looks there)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET=""
PROFILE=debug
OUT=""
ONLY=""
while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGET="${2:?--target needs a triple}"; shift ;;
    --release) PROFILE=release ;;
    --only) ONLY="${2:?--only needs plugin names}"; shift ;;
    -h|--help) sed -n '2,7p' "$0"; exit 0 ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) OUT="$1" ;;
  esac
  shift
done
[ -n "$OUT" ] || { echo "usage: stage-plugins.sh [--target <triple>] [--release] <out_dir>" >&2; exit 2; }
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

PLUGINS=(clock http weather)
[ -z "$ONLY" ] || IFS=, read -r -a PLUGINS <<< "$ONLY"
ARGS=()
[ -n "$TARGET" ] && ARGS+=(--target "$TARGET")
[ "$PROFILE" = release ] && ARGS+=(--release)
PKGS=()
for p in "${PLUGINS[@]}"; do PKGS+=(-p "notideck-plugin-$p"); done
(cd "$REPO/host" && cargo build "${PKGS[@]}" ${ARGS[@]+"${ARGS[@]}"})

BIN="$REPO/host/target"
[ -n "$TARGET" ] && BIN="$BIN/$TARGET"
BIN="$BIN/$PROFILE"
EXT=""
case "$TARGET" in *windows*) EXT=".exe" ;; esac
[ -z "$TARGET" ] && case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) EXT=".exe" ;; esac
STRIP=""
case "$TARGET" in *windows-gnu*) command -v x86_64-w64-mingw32-strip >/dev/null && STRIP=x86_64-w64-mingw32-strip ;; esac

for p in "${PLUGINS[@]}"; do
  src="$REPO/host/plugins/$p"
  id="$(sed -n 's/^ *"id": *"\([^"]*\)",$/\1/p' "$src/manifest.json" | head -1)"
  [ -n "$id" ] || { echo "no id in $src/manifest.json" >&2; exit 1; }
  dest="$OUT/$id"
  rm -rf "$dest"
  mkdir -p "$dest"
  cp "$src/manifest.json" "$dest/"
  cp -r "$src/icons" "$dest/icons"
  cp "$BIN/$p$EXT" "$dest/$p$EXT"
  [ -z "$STRIP" ] || "$STRIP" "$dest/$p$EXT"
  echo "staged $id"
done
