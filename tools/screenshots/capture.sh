#!/usr/bin/env bash
# Regenerates the README screenshots in docs/images/.
#
# Builds a demo library (handwriting from the Hershey script stroke font,
# typed text, highlights, sketches and an annotated PDF), opens it in the
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

DISPLAY="$DISPLAY_NUM" GDK_SCALE=2 E2E_SETTLE=6 \
  python3 tools/linux-e2e/e2e.py tools/screenshots/session.txt "$WORK/shots" "$WORK/library"

mkdir -p "$OUT"
for name in editor library pdf-annotation dark-theme epaper-theme; do
  # captured at 2x; stored at 1600 px wide to keep the repository small
  convert "$WORK/shots/$name.png" -resize 1600x -strip "$OUT/$name.png"
done
# the library only fills its top rows
convert "$OUT/library.png" -gravity north -crop 1600x560+0+0 +repage "$OUT/library.png"
echo "screenshots written to $OUT"
