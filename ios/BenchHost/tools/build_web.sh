#!/usr/bin/env bash
# Builds the Tauri frontend and copies dist/ into the iOS app bundle resources.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
(cd "$here/../../reference/m00-tauri-app" && npm run build)
rm -rf "$here/Sources/BenchHost/web"
cp -r "$here/../../reference/m00-tauri-app/dist" "$here/Sources/BenchHost/web"
cp "$here/../../tests/fixtures/sample-strokes.json" "$here/Sources/BenchHost/fixtures/"
echo "web bundle -> Sources/BenchHost/web"
