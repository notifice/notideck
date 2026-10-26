#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the Linux tarball and AppImage of Notideck (x86_64, native window + tray).
#   scripts/build-linux.sh [--no-appimage] [--refresh-adb]
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG="$REPO/packaging/linux"
VENDOR="$PKG/vendor"
PT_URL="https://dl.google.com/android/repository/platform-tools-latest-linux.zip"
AIT_VER="1.9.1"
AIT_URL="https://github.com/AppImage/appimagetool/releases/download/$AIT_VER/appimagetool-x86_64.AppImage"
APPIMAGE=1
REFRESH=0
for a in "$@"; do
  case "$a" in
    --no-appimage) APPIMAGE=0 ;;
    --refresh-adb) REFRESH=1 ;;
    -h|--help) sed -n '2,4p' "$0"; exit 0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$REPO/host/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "cannot read version from host/Cargo.toml" >&2; exit 1; }
NAME="Notideck-$VERSION-linux-x86_64"
DIST="$REPO/dist"

# --- platform-tools (cached) ---
mkdir -p "$VENDOR"
printf '*\n!.gitignore\n' > "$VENDOR/.gitignore"
PT_ZIP="$VENDOR/platform-tools-linux.zip"
if [ "$REFRESH" = 1 ] || [ ! -f "$PT_ZIP" ]; then
  echo "Downloading Android platform-tools..."
  curl -fL --retry 3 -o "$PT_ZIP.part" "$PT_URL"
  mv "$PT_ZIP.part" "$PT_ZIP"
  rm -rf "$VENDOR/platform-tools"
fi
if [ ! -d "$VENDOR/platform-tools" ]; then
  python3 -I - "$PT_ZIP" "$VENDOR" <<'PY'
import os, sys, zipfile
z = zipfile.ZipFile(sys.argv[1])
for n in ("platform-tools/adb", "platform-tools/NOTICE.txt"):
    z.extract(n, sys.argv[2])
os.chmod(os.path.join(sys.argv[2], "platform-tools/adb"), 0o755)
PY
fi

# --- build ---
echo "Building nd-agent (release, native window + tray; needs webkit2gtk-4.1, gtk3 and ayatana-appindicator dev packages)..."
(cd "$REPO/host" && cargo build --release -p nd-agent --features nd-agent/linux-gui)
EXE="$REPO/host/target/release/nd-agent"
[ -f "$EXE" ] || { echo "error: $EXE was not built" >&2; exit 1; }

# --- common staging: a prefix tree (bin, lib, share) ---
STAGE="$DIST/.stage-linux"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/share/icons/hicolor/256x256/apps" "$STAGE/share/applications" "$STAGE/share/notideck"
install -m755 "$EXE" "$STAGE/bin/notideck"
strip "$STAGE/bin/notideck"
install -m755 "$VENDOR/platform-tools/adb" "$STAGE/bin/adb"
strip "$STAGE/bin/adb" 2>/dev/null || true
echo "Building bundled plugins (release)..."
"$REPO/scripts/stage-plugins.sh" --release "$STAGE/lib/notideck/plugins"
for d in "$STAGE"/lib/notideck/plugins/*/; do
  for f in "$d"*; do [ -f "$f" ] && [ -x "$f" ] && strip "$f" 2>/dev/null || true; done
done
install -m644 "$PKG/notideck.desktop" "$STAGE/share/applications/notideck.desktop"
install -m644 "$PKG/notideck.png" "$STAGE/share/icons/hicolor/256x256/apps/notideck.png"
install -m644 "$PKG/70-notideck-uinput.rules" "$PKG/notideck-agent.service" "$STAGE/share/notideck/"
install -m644 "$VENDOR/platform-tools/NOTICE.txt" "$STAGE/share/notideck/adb-NOTICE.txt"
# Licences of the bundled open-source code (MIT/Apache/BSD notices must travel with the binary).
mkdir -p "$STAGE/share/doc/notideck"
"$REPO/scripts/third-party-licenses.sh" "$STAGE/share/doc/notideck/THIRD_PARTY_LICENSES.txt" --target x86_64-unknown-linux-gnu
install -m644 "$REPO/THIRD_PARTY.md" "$STAGE/share/doc/notideck/THIRD_PARTY.md"

# --- tarball ---
OUT="$DIST/$NAME"
rm -rf "$OUT" "$DIST/$NAME.tar.gz"
mkdir -p "$OUT"
cp -a "$STAGE/bin" "$STAGE/lib" "$STAGE/share" "$OUT/"
cp "$REPO/LICENSE" "$OUT/LICENSE"
cp "$STAGE/share/doc/notideck/THIRD_PARTY_LICENSES.txt" "$STAGE/share/doc/notideck/THIRD_PARTY.md" "$OUT/"
install -m755 "$PKG/install.sh" "$PKG/uninstall.sh" "$OUT/"
sed "s/@VERSION@/$VERSION/g" "$PKG/README.txt" > "$OUT/README.txt"
tar -C "$DIST" --owner=0 --group=0 --numeric-owner -czf "$DIST/$NAME.tar.gz" "$NAME"
rm -rf "$OUT"
echo "Tarball:  $DIST/$NAME.tar.gz ($(du -h "$DIST/$NAME.tar.gz" | cut -f1))"

# --- AppImage ---
if [ "$APPIMAGE" = 1 ]; then
  AIT="$VENDOR/appimagetool-$AIT_VER-x86_64.AppImage"
  if [ ! -x "$AIT" ]; then
    echo "Downloading appimagetool $AIT_VER..."
    curl -fL --retry 3 -o "$AIT.part" "$AIT_URL"
    chmod +x "$AIT.part"; mv "$AIT.part" "$AIT"
  fi
  APPDIR="$DIST/.Notideck.AppDir"
  rm -rf "$APPDIR"
  mkdir -p "$APPDIR/usr"
  cp -a "$STAGE/bin" "$STAGE/lib" "$STAGE/share" "$APPDIR/usr/"
  cp "$PKG/notideck.desktop" "$PKG/notideck.png" "$APPDIR/"
  ln -s notideck.png "$APPDIR/.DirIcon"
  cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
# Notideck AppImage entry point. Plugins are found at <exe dir>/../lib/notideck/plugins by the agent.
HERE="$(dirname "$(readlink -f "$0")")"
if [ "$#" -eq 0 ]; then
  exec "$HERE/usr/bin/notideck" --open
fi
exec "$HERE/usr/bin/notideck" "$@"
RUN
  chmod +x "$APPDIR/AppRun"
  rm -f "$DIST/Notideck-$VERSION-x86_64.AppImage"
  ARCH=x86_64 "$AIT" --appimage-extract-and-run --no-appstream "$APPDIR" "$DIST/Notideck-$VERSION-x86_64.AppImage"
  rm -rf "$APPDIR"
  echo "AppImage: $DIST/Notideck-$VERSION-x86_64.AppImage ($(du -h "$DIST/Notideck-$VERSION-x86_64.AppImage" | cut -f1))"
fi
rm -rf "$STAGE"
