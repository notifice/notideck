#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Cross-build the portable Windows package of Notideck from Linux/WSL.
#   scripts/build-windows.sh [--refresh-adb] [--copy-to-desktop] [--apk <path>] [--no-installer]
#     --apk <path>    phone APK to bundle as notideck-phone.apk (default:
#                     android/app/build/outputs/apk/debug/app-debug.apk if present)
#     --no-installer  skip the NSIS setup.exe (needs makensis: sudo apt install nsis)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET=x86_64-pc-windows-gnu
VENDOR="$REPO/packaging/windows/vendor"
PT_URL="https://dl.google.com/android/repository/platform-tools-latest-windows.zip"
REFRESH=0
DESKTOP=0
APK=""
INSTALLER=1
while [ $# -gt 0 ]; do
  case "$1" in
    --refresh-adb) REFRESH=1 ;;
    --copy-to-desktop) DESKTOP=1 ;;
    --no-installer) INSTALLER=0 ;;
    --apk) [ $# -ge 2 ] || { echo "--apk needs a path" >&2; exit 2; }; APK="$2"; shift ;;
    -h|--help) sed -n '2,7p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

missing=0
if ! command -v x86_64-w64-mingw32-gcc >/dev/null; then
  echo "missing: mingw-w64 toolchain. Install: sudo apt install mingw-w64" >&2; missing=1
fi
if ! command -v rustup >/dev/null || ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "missing: rust target $TARGET. Install: rustup target add $TARGET" >&2; missing=1
fi
[ "$missing" = 0 ] || exit 1

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$REPO/host/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "cannot read version from host/Cargo.toml" >&2; exit 1; }
NAME="Notideck-$VERSION-windows-x64"

# --- platform-tools (cached) ---
mkdir -p "$VENDOR"
PT_ZIP="$VENDOR/platform-tools-windows.zip"
if [ "$REFRESH" = 1 ] || [ ! -f "$PT_ZIP" ]; then
  echo "Downloading Android platform-tools..."
  curl -fL --retry 3 -o "$PT_ZIP.part" "$PT_URL"
  mv "$PT_ZIP.part" "$PT_ZIP"
  rm -rf "$VENDOR/platform-tools"
fi
if [ ! -d "$VENDOR/platform-tools" ]; then
  python3 -I - "$PT_ZIP" "$VENDOR" <<'PY'
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1])
for n in ("platform-tools/adb.exe", "platform-tools/AdbWinApi.dll",
          "platform-tools/AdbWinUsbApi.dll", "platform-tools/NOTICE.txt"):
    z.extract(n, sys.argv[2])
PY
fi

# --- build ---
echo "Building nd-agent (release, $TARGET)..."
(cd "$REPO/host" && cargo build --release -p nd-agent -p nd-vcam-dll --target "$TARGET")
EXE="$REPO/host/target/$TARGET/release/nd-agent.exe"
VCAM_DLL="$REPO/host/target/$TARGET/release/notideck_vcam.dll"
[ -f "$VCAM_DLL" ] || { echo "error: $VCAM_DLL was not built" >&2; exit 1; }

# --- assemble ---
DIST="$REPO/dist"
OUT="$DIST/$NAME"
rm -rf "$OUT" "$DIST/$NAME.zip"
mkdir -p "$OUT"
cp "$EXE" "$OUT/Notideck.exe"
x86_64-w64-mingw32-strip "$OUT/Notideck.exe"
# The virtual camera (Media Foundation media source). Notideck.exe --install-vcam registers it.
cp "$VCAM_DLL" "$OUT/notideck_vcam.dll"
x86_64-w64-mingw32-strip "$OUT/notideck_vcam.dll"
cp "$VENDOR/platform-tools/adb.exe" "$VENDOR/platform-tools/AdbWinApi.dll" \
   "$VENDOR/platform-tools/AdbWinUsbApi.dll" "$OUT/"
cp "$VENDOR/platform-tools/NOTICE.txt" "$OUT/adb-NOTICE.txt"
# wry links WebView2Loader.dll dynamically on the GNU target; ship the one from the locked
# webview2-com-sys crate (BSD-licensed redistributable from Microsoft's WebView2 SDK).
WV2_VER=$(awk '/^name = "webview2-com-sys"/{getline; gsub(/version = |"/,""); print; exit}' "$REPO/host/Cargo.lock")
WV2_DLL=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -path "*webview2-com-sys-$WV2_VER/x64/WebView2Loader.dll" | head -1)
if [[ -z "$WV2_DLL" ]]; then
  echo "error: WebView2Loader.dll for webview2-com-sys $WV2_VER not found in the cargo registry" >&2
  exit 1
fi
cp "$WV2_DLL" "$OUT/"
cp "$REPO/LICENSE" "$OUT/LICENSE"
# Phone app (debug APK): --apk, else the Gradle debug output, else build without it.
[ -n "$APK" ] || APK="$REPO/android/app/build/outputs/apk/debug/app-debug.apk"
if [ -f "$APK" ]; then
  cp "$APK" "$OUT/notideck-phone.apk"
else
  echo "warning: no phone APK found ($APK); building without notideck-phone.apk" >&2
fi
# Bundled plugins (clock, http, weather): plugins/<id>/{manifest.json, exe, icons/} next to Notideck.exe.
echo "Building bundled plugins (release, $TARGET)..."
"$REPO/scripts/stage-plugins.sh" --target "$TARGET" --release "$OUT/plugins"
cat > "$OUT/README.txt" <<TXT
Notideck $VERSION (Windows)
===========================
Notideck turns an old Android phone into a Stream Deck-style controller.
Recommended install: run Notideck-$VERSION-windows-x64-setup.exe (adds a Start menu
entry, an uninstaller and the optional webcam driver). This portable zip needs no
installer: unpack it anywhere and run Notideck.exe.

This folder is the whole app. adb.exe (Google's Android platform-tools, see
adb-NOTICE.txt) is bundled so the phone can connect over USB.

First run
  1. Install the Notideck app on the phone (notideck-phone.apk in this folder, if
     present; Notideck can also install it over USB from its setup screen).
  2. Double-click Notideck.exe. A window opens and an icon appears in the
     system tray. Closing the window keeps Notideck running in the tray.
  3. Plug the phone in with a USB cable and accept the "Allow USB debugging?"
     prompt on the phone (tick "Always allow").

Enable USB debugging on the phone
  Settings > About phone > tap "Build number" 7 times, then
  Settings > System > Developer options > turn on "USB debugging".

Use the phone as a webcam (Windows 11)
  Open the Camera tab in Notideck and press "Install webcam (needs admin once)".
  Windows asks for administrator permission one time to register "Notideck
  Camera"; keep notideck_vcam.dll next to Notideck.exe. After that, pick
  "Notideck Camera" in Zoom, Teams, Discord, OBS or the Windows Camera app. The
  phone camera starts by itself when an app opens it and stops 5 seconds after
  it closes. (Advanced: Settings > Webcam driver > Uninstall, or run
  "Notideck.exe --uninstall-vcam" as administrator.)

Plugins
  The plugins folder holds the bundled plugins (clock and timer, HTTP request,
  weather). Notideck > Plugins lists them. See docs/PLUGINS.md to write your own.

Where things live
  Profiles, settings, paired phones, plugins: %APPDATA%\\Notideck
  Logs: %LOCALAPPDATA%\\Notideck
  (Notideck > Settings > Storage opens them.)

Uninstall
  Installed with the setup program: Settings > Apps > Notideck > Uninstall.
  Portable zip:
  1. Open Notideck > Settings and untick "Start at login".
  2. Quit Notideck from the tray menu.
  3. Delete this folder, %APPDATA%\\Notideck and %LOCALAPPDATA%\\Notideck.

Licence: GPL-3.0-or-later (see LICENSE).
TXT
sed -i 's/$/\r/' "$OUT/README.txt"

# --- zip ---
if command -v zip >/dev/null; then
  (cd "$DIST" && zip -qr "$NAME.zip" "$NAME")
else
  python3 -I - "$DIST" "$NAME" <<'PY'
import os, sys, zipfile
d, n = sys.argv[1:3]
with zipfile.ZipFile(os.path.join(d, n + ".zip"), "w", zipfile.ZIP_DEFLATED) as z:
    for root, _, files in os.walk(os.path.join(d, n)):
        for f in files:
            p = os.path.join(root, f)
            z.write(p, os.path.relpath(p, d))
PY
fi

# --- installer (NSIS) ---
SETUP=""
if [ "$INSTALLER" = 1 ]; then
  if command -v makensis >/dev/null; then
    SETUP="$DIST/$NAME-setup.exe"
    rm -f "$SETUP"
    makensis -V2 -INPUTCHARSET UTF8 \
      -DVERSION="$VERSION" -DSTAGE_DIR="$OUT" -DOUT_FILE="$SETUP" \
      -DICON_FILE="$REPO/host/crates/nd-agent/res/notideck.ico" \
      -DLICENSE_FILE="$REPO/LICENSE" \
      "$REPO/packaging/windows/notideck.nsi"
  else
    echo "warning: makensis not found; skipping the installer. Install: sudo apt install nsis" >&2
  fi
fi

if [ "$DESKTOP" = 1 ]; then
  WINUSER="$(cmd.exe /c 'echo %USERNAME%' 2>/dev/null | tr -d '\r\n')"
  DEST="/mnt/c/Users/$WINUSER/Desktop"
  [ -d "$DEST" ] || { echo "Desktop not found: $DEST" >&2; exit 1; }
  rm -rf "$DEST/$NAME"
  cp -r "$OUT" "$DEST/"
  echo "Copied to: $DEST/$NAME"
fi

echo
echo "Folder: $OUT"
echo "Zip:    $DIST/$NAME.zip ($(du -h "$DIST/$NAME.zip" | cut -f1))"
[ -z "$SETUP" ] || echo "Setup:  $SETUP ($(du -h "$SETUP" | cut -f1))"
