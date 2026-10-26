#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Remove a Notideck installation made by install.sh.
#   uninstall.sh [--prefix DIR | --system] [--purge]
# --purge also deletes ~/.config/notideck (profiles, settings, paired phones) and its data.
set -euo pipefail

PREFIX=""
PURGE=0
SYSTEM=0
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift ;;
    --system) SYSTEM=1; PREFIX=/usr/local ;;
    --purge) PURGE=1 ;;
    -h|--help) sed -n '2,5p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
if [ -z "$PREFIX" ]; then
  # Run from <prefix>/share/notideck/, or default to ~/.local.
  HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  if [ -f "$HERE/installed-files" ]; then PREFIX="$(cd "$HERE/../.." && pwd)"; else PREFIX="$HOME/.local"; fi
fi
MAN="$PREFIX/share/notideck/installed-files"
if [ ! -f "$MAN" ]; then
  echo "Nothing to uninstall: $MAN not found (was Notideck installed with a different --prefix?)."
  exit 0
fi

SUDO=""
if [ ! -w "$PREFIX" ] && [ "$(id -u)" != 0 ]; then
  command -v sudo >/dev/null || { echo "error: $PREFIX is not writable and sudo is missing" >&2; exit 1; }
  SUDO=sudo
fi

# Stop the agent and the service first (best effort).
if command -v systemctl >/dev/null; then
  systemctl --user disable --now notideck-agent >/dev/null 2>&1 || true
fi
if [ -x "$PREFIX/bin/notideck" ]; then "$PREFIX/bin/notideck" --quit >/dev/null 2>&1 || true; fi

echo "Removing Notideck from $PREFIX"
while IFS= read -r f; do
  [ -n "$f" ] || continue
  case "$f" in /etc/udev/*) rmcmd=(sudo rm -f) ;; "$HOME"/*) rmcmd=(rm -f) ;; *) rmcmd=($SUDO rm -f) ;; esac
  if [ -e "$f" ] || [ -L "$f" ]; then "${rmcmd[@]}" "$f" && echo "  removed $f"; fi
  case "$f" in /etc/udev/*) sudo udevadm control --reload >/dev/null 2>&1 || true ;; esac
done < "$MAN"
# Webcam config written by --install-vcam (never touches other v4l2loopback devices' own configs).
for f in /etc/modprobe.d/notideck-v4l2loopback.conf /etc/modules-load.d/notideck-v4l2loopback.conf; do
  if [ -e "$f" ]; then
    if [ "$(id -u)" = 0 ]; then rm -f "$f"; else sudo rm -f "$f"; fi && echo "  removed $f"
  fi
done
$SUDO rm -f "$MAN" "$PREFIX/share/notideck/uninstall.sh"

# Remove the generated icon cache and now-empty directories (never the prefix itself).
$SUDO rm -f "$PREFIX/share/icons/hicolor/icon-theme.cache"
for d in lib share/notideck share/doc/notideck share/icons share/applications; do
  if [ -d "$PREFIX/$d" ]; then $SUDO find "$PREFIX/$d" -depth -type d -empty -delete 2>/dev/null || true; fi
done
systemctl --user daemon-reload >/dev/null 2>&1 || true
if command -v update-desktop-database >/dev/null; then
  $SUDO update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
fi

if [ "$PURGE" = 1 ]; then
  for d in "${XDG_CONFIG_HOME:-$HOME/.config}/notideck" "${XDG_DATA_HOME:-$HOME/.local/share}/notideck" \
           "${XDG_STATE_HOME:-$HOME/.local/state}/notideck"; do
    if [ -d "$d" ]; then rm -rf "$d"; echo "  purged $d"; fi
  done
else
  echo "Kept your settings in ~/.config/notideck (use --purge to delete them)."
fi
echo "Notideck removed."
