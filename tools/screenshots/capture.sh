#!/usr/bin/env bash
# Regenerates the README and user manual screenshots in docs/images/.
#
# Builds a demo library (handwriting from the Hershey script stroke font,
# typed text, highlights, sketches, an annotated PDF, a lab write-up with a
# spreadsheet table and an infinite canvas), opens it in the
# release app inside a nested Xephyr display at 2x scale, drives the UI with
# tools/linux-e2e (XTest) and saves the captures.
#
# Needs: Linux, Xephyr, ImageMagick (`import`, `convert`), a C compiler and
# libX11/libXtst headers (for xdrive), plus the normal app build toolchain.
#
#   tools/screenshots/capture.sh            # build the app if missing, capture
#   SKIP_BUILD=1 tools/screenshots/capture.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
OUT="$ROOT/docs/images"
WORK="$(mktemp -d)"
DISPLAY_NUM="${CAPTURE_DISPLAY:-:47}"
trap 'kill "${XEPHYR_PID:-}" 2>/dev/null || true; rm -rf "$WORK"' EXIT

if [[ -z "${SKIP_BUILD:-}" || ! -x target/release/penroam-app ]]; then
  pnpm tauri build --no-bundle
fi
[[ -x tools/linux-e2e/xdrive ]] || cc -O2 -o tools/linux-e2e/xdrive tools/linux-e2e/xdrive.c -lX11 -lXtst

python3 -I tools/screenshots/make_pdf.py "$WORK/reading.pdf"
cargo run -q -p penroam-storage --example demo_library -- "$WORK/library" "$WORK/reading.pdf"

Xephyr "$DISPLAY_NUM" -screen 2560x1800 -ac -br -noreset >/dev/null 2>&1 &
XEPHYR_PID=$!
sleep 2

# Isolated XDG folders: the session changes the theme, and WebKit keeps the
# app's settings (localStorage) under XDG_DATA_HOME. Without this the run would
# change the theme of the developer's own Penroam install.
mkdir -p "$WORK/xdg/data" "$WORK/xdg/cache" "$WORK/xdg/config"
DISPLAY="$DISPLAY_NUM" GDK_SCALE=2 E2E_SETTLE=6 \
  XDG_DATA_HOME="$WORK/xdg/data" XDG_CACHE_HOME="$WORK/xdg/cache" XDG_CONFIG_HOME="$WORK/xdg/config" \
  python3 tools/linux-e2e/e2e.py tools/screenshots/session.txt "$WORK/shots" "$WORK/library"

# every image the session takes; all or nothing, so a failed run never leaves
# the docs with a mix of old and new images
shots=$(awk '$1 == "shot" { print $2 }' tools/screenshots/session.txt)
for name in $shots; do
  [[ -f "$WORK/shots/$name.png" ]] || { echo "session did not take $name" >&2; exit 1; }
done
mkdir -p "$OUT"
for name in $shots; do
  # captured at 2x; stored at 62.5% (a full window is 1600 px wide) to keep the repository small
  convert "$WORK/shots/$name.png" -resize 62.5% -strip "$OUT/$name.png"
done
echo "screenshots written to $OUT"
