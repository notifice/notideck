#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Run the whole release pipeline: lint, tests, Android build, Windows + Linux packages, checksums.
#   scripts/release.sh [--skip-android] [--skip-windows] [--skip-linux] [--skip-checks]
#     --skip-checks   skip fmt/clippy/cargo test/Android unit tests (builds only)
# Windows tests run natively through WSL interop (NOTIDECK_NO_ADB=1). Output goes to dist/.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$REPO/dist"
WIN_TARGET="x86_64-pc-windows-gnu"
ANDROID=1; WINDOWS=1; LINUX=1; CHECKS=1
for a in "$@"; do
  case "$a" in
    --skip-android) ANDROID=0 ;;
    --skip-windows) WINDOWS=0 ;;
    --skip-linux) LINUX=0 ;;
    --skip-checks) CHECKS=0 ;;
    -h|--help) sed -n '2,6p' "$0"; exit 0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$REPO/host/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "cannot read version from host/Cargo.toml" >&2; exit 1; }
APK_SRC="$REPO/android/app/build/outputs/apk/debug/app-debug.apk"
APK_OUT="$DIST/Notideck-$VERSION-phone.apk"

N=0
step() { N=$((N + 1)); printf '\n==================== [%s] %s ====================\n' "$N" "$*"; }
trap 'rc=$?; [ $rc -eq 0 ] || printf "\nRELEASE FAILED (exit %s) during step [%s]\n" "$rc" "$N" >&2' EXIT

echo "Notideck release $VERSION (android=$ANDROID windows=$WINDOWS linux=$LINUX checks=$CHECKS)"

if [ "$CHECKS" = 1 ]; then
  step "cargo fmt --check"
  (cd "$REPO/host" && cargo fmt --all -- --check)

  step "cargo clippy (Linux)"
  (cd "$REPO/host" && cargo clippy --workspace --all-targets -- -D warnings)

  step "cargo clippy ($WIN_TARGET)"
  (cd "$REPO/host" && cargo clippy --workspace --all-targets --target "$WIN_TARGET" -- -D warnings)

  step "cargo test (Linux)"
  (cd "$REPO/host" && NOTIDECK_NO_ADB=1 cargo test --workspace)

  if [ "$WINDOWS" = 1 ]; then
    step "cargo test ($WIN_TARGET, native via WSL interop)"
    (cd "$REPO/host" && NOTIDECK_NO_ADB=1 cargo test --workspace --target "$WIN_TARGET")
  fi
fi

if [ "$ANDROID" = 1 ]; then
  step "Android: clean, assembleDebug$([ "$CHECKS" = 1 ] && echo ", unit tests")"
  export JAVA_TOOL_OPTIONS="${JAVA_TOOL_OPTIONS:+$JAVA_TOOL_OPTIONS }-Djava.net.preferIPv4Stack=true"
  tasks=(:app:clean :app:assembleDebug)
  [ "$CHECKS" = 1 ] && tasks+=(:app:testDebugUnitTest)
  (cd "$REPO/android" && ./gradlew --no-daemon "${tasks[@]}")
fi

mkdir -p "$DIST"
APK_ARG=()
if [ "$ANDROID" = 1 ] || [ -f "$APK_SRC" ]; then
  [ -f "$APK_SRC" ] || { echo "phone APK not found: $APK_SRC" >&2; exit 1; }
  APK_ARG=(--apk "$APK_SRC")
fi

if [ "$WINDOWS" = 1 ]; then
  step "Windows package (zip + installer)"
  "$REPO/scripts/build-windows.sh" "${APK_ARG[@]}"
fi

if [ "$LINUX" = 1 ]; then
  step "Linux package (tarball + AppImage)"
  "$REPO/scripts/build-linux.sh"
fi

if [ ${#APK_ARG[@]} -gt 0 ]; then
  step "Phone APK"
  cp "$APK_SRC" "$APK_OUT"
  echo "$APK_OUT"
fi

step "Third-party licences (combined, whole dependency tree)"
TPL_OUT="$DIST/Notideck-$VERSION-THIRD_PARTY_LICENSES.txt"
"$REPO/scripts/third-party-licenses.sh" "$TPL_OUT"

step "Checksums"
ARTEFACTS=()
for f in "$DIST/Notideck-$VERSION-windows-x64-setup.exe" \
         "$DIST/Notideck-$VERSION-windows-x64.zip" \
         "$DIST/Notideck-$VERSION-linux-x86_64.tar.gz" \
         "$DIST/Notideck-$VERSION-x86_64.AppImage" \
         "$APK_OUT" \
         "$TPL_OUT"; do
  [ -f "$f" ] && ARTEFACTS+=("$(basename "$f")")
done
[ ${#ARTEFACTS[@]} -gt 0 ] || { echo "no release artefacts found in $DIST" >&2; exit 1; }
(cd "$DIST" && sha256sum "${ARTEFACTS[@]}" > SHA256SUMS && cat SHA256SUMS)

step "Summary"
printf '%-52s %10s\n' ARTEFACT SIZE
for f in "${ARTEFACTS[@]}" SHA256SUMS; do
  printf '%-52s %10s\n' "$f" "$(du -h "$DIST/$f" | cut -f1)"
done
echo
echo "Release $VERSION ready in $DIST"
