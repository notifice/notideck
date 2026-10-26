#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Install Notideck from this folder.
#   ./install.sh [--prefix DIR | --system] [--yes] [--with-udev] [--with-service] [--with-webcam]
# Default prefix is ~/.local; --system installs to /usr/local using sudo.
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="$HOME/.local"
SYSTEM=0
YES=0
UDEV=ask
SERVICE=ask
WEBCAM=ask
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift ;;
    --system) SYSTEM=1; PREFIX=/usr/local ;;
    --yes|-y) YES=1 ;;
    --with-udev) UDEV=yes ;;
    --with-service) SERVICE=yes ;;
    --with-webcam) WEBCAM=yes ;;
    -h|--help) sed -n '2,5p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
if [ "$YES" = 1 ]; then
  [ "$UDEV" != ask ] || UDEV=no
  [ "$SERVICE" != ask ] || SERVICE=no
  [ "$WEBCAM" != ask ] || WEBCAM=no
fi
[ -f "$SRC/bin/notideck" ] || { echo "error: run install.sh from the extracted Notideck folder" >&2; exit 1; }
mkdir -p "$PREFIX"
PREFIX="$(cd "$PREFIX" && pwd)"

SUDO=""
if [ "$SYSTEM" = 1 ] && [ "$(id -u)" != 0 ]; then
  command -v sudo >/dev/null || { echo "error: --system needs sudo" >&2; exit 1; }
  SUDO=sudo
fi

ask() { # ask "question" -> 0 for yes (default no)
  local r
  read -r -p "$1 [y/N] " r || return 1
  case "$r" in y|Y|yes|YES) return 0 ;; *) return 1 ;; esac
}

echo "Installing Notideck to $PREFIX"
LIST="$(mktemp)"
trap 'rm -f "$LIST"' EXIT
inst() { # inst MODE SRC DEST
  $SUDO install -Dm"$1" "$2" "$3"
  echo "$3" >> "$LIST"
}

inst 755 "$SRC/bin/notideck" "$PREFIX/bin/notideck"
inst 755 "$SRC/bin/adb" "$PREFIX/bin/adb"
(cd "$SRC/lib/notideck/plugins" && find . -type f | sort) | while read -r f; do
  m=644; if [ -x "$SRC/lib/notideck/plugins/$f" ]; then m=755; fi
  inst "$m" "$SRC/lib/notideck/plugins/$f" "$PREFIX/lib/notideck/plugins/${f#./}"
done
sed -e "s#^Exec=.*#Exec=$PREFIX/bin/notideck --open#" -e 's#^Icon=.*#Icon=notideck#' \
  "$SRC/share/applications/notideck.desktop" > "$LIST.desktop"
inst 644 "$LIST.desktop" "$PREFIX/share/applications/notideck.desktop"
rm -f "$LIST.desktop"
inst 644 "$SRC/share/icons/hicolor/256x256/apps/notideck.png" "$PREFIX/share/icons/hicolor/256x256/apps/notideck.png"
inst 644 "$SRC/share/notideck/adb-NOTICE.txt" "$PREFIX/share/notideck/adb-NOTICE.txt"
inst 644 "$SRC/share/notideck/70-notideck-uinput.rules" "$PREFIX/share/notideck/70-notideck-uinput.rules"
inst 644 "$SRC/share/notideck/notideck-agent.service" "$PREFIX/share/notideck/notideck-agent.service"
inst 644 "$SRC/LICENSE" "$PREFIX/share/notideck/LICENSE"

# Desktop caches (best effort).
if command -v update-desktop-database >/dev/null; then
  $SUDO update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null; then
  $SUDO gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
fi

# --- optional: udev rule for keyboard injection (/dev/uinput) ---
if [ "$UDEV" = ask ]; then
  if ask "Install the udev rule that lets Notideck type keys and press hotkeys (needs sudo)?"; then UDEV=yes; else UDEV=no; fi
fi
if [ "$UDEV" = yes ]; then
  RULE=/etc/udev/rules.d/70-notideck-uinput.rules
  echo "Installing $RULE (sudo)..."
  sudo install -Dm644 "$SRC/share/notideck/70-notideck-uinput.rules" "$RULE"
  echo "$RULE" >> "$LIST.root"
  sudo udevadm control --reload || true
  sudo udevadm trigger || true
  if ! id -nG "$USER" | tr ' ' '\n' | grep -qx input; then
    sudo usermod -aG input "$USER" && echo "Added $USER to the 'input' group."
  fi
  echo "Log out and back in so the group change takes effect."
  echo "(If /dev/uinput is missing: sudo modprobe uinput)"
fi

# --- optional: webcam (v4l2loopback device "Notideck Camera") ---
if [ "$WEBCAM" = ask ]; then
  if ask "Set up the Notideck webcam (v4l2loopback; writes /etc/modprobe.d and /etc/modules-load.d, needs sudo)?"; then WEBCAM=yes; else WEBCAM=no; fi
fi
if [ "$WEBCAM" = yes ]; then
  # uninstall.sh removes the two config files by name, so they are not in the manifest.
  if ! sudo "$PREFIX/bin/notideck" --install-vcam; then
    echo "warning: webcam setup failed (is the v4l2loopback kernel module installed? Arch/CachyOS/Debian/Ubuntu: v4l2loopback-dkms)." >&2
    echo "         You can retry later from Notideck > Camera." >&2
  fi
fi

# --- optional: systemd user service ---
if [ "$SERVICE" = ask ]; then
  if ask "Start Notideck automatically at login (systemd user service)?"; then SERVICE=yes; else SERVICE=no; fi
fi
if [ "$SERVICE" = yes ]; then
  if command -v systemctl >/dev/null; then
    UNIT="$HOME/.config/systemd/user/notideck-agent.service"
    mkdir -p "$(dirname "$UNIT")"
    sed "s#^ExecStart=.*#ExecStart=$PREFIX/bin/notideck --headless#" "$SRC/share/notideck/notideck-agent.service" > "$UNIT"
    echo "$UNIT" >> "$LIST.user"
    systemctl --user daemon-reload || true
    systemctl --user enable --now notideck-agent \
      || echo "warning: could not enable the service; try: systemctl --user enable --now notideck-agent" >&2
  else
    echo "warning: systemctl not found; skipping the service" >&2
  fi
fi

# --- manifest for uninstall.sh (merge with any previous install into the same prefix) ---
MAN="$PREFIX/share/notideck/installed-files"
{
  if [ -f "$MAN" ]; then cat "$MAN"; fi
  cat "$LIST"
  if [ -f "$LIST.root" ]; then cat "$LIST.root"; fi
  if [ -f "$LIST.user" ]; then cat "$LIST.user"; fi
} | awk '!s[$0]++' > "$LIST.man"
$SUDO install -Dm644 "$LIST.man" "$MAN"
rm -f "$LIST.man" "$LIST.root" "$LIST.user"
# uninstall.sh is kept next to the manifest so it can be run later.
$SUDO install -Dm755 "$SRC/uninstall.sh" "$PREFIX/share/notideck/uninstall.sh"

echo
echo "Done. Start Notideck from your app launcher, or run: $PREFIX/bin/notideck --open"
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "Note: $PREFIX/bin is not on your PATH." ;; esac
echo "To remove it: $PREFIX/share/notideck/uninstall.sh [--prefix $PREFIX] [--purge]"
