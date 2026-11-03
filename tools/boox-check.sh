#!/usr/bin/env bash
# On-device check for the BOOX native ink path. Needs an UNLOCKED BOOX
# on adb (keep it awake: the script sends wake key events while it runs).
#
#   tools/boox-check.sh [path/to/app-universal-release.apk]
#
# Part 1 (automatic): native surface mount, a stroke committed through the
# native post-pen-up path (injected samples: NOT a real pen), web model and
# Rust persistence, undo/redo sync to the native layer, native/web layer
# switch (screenshots A/B for alignment), 1k/5k/10k native loads.
# Part 2 (manual, printed at the end): real pen strokes and the two-OS
# .penroam transfer.
set -euo pipefail
export PATH="$HOME/Android/sdk/platform-tools:$PATH"
cd "$(dirname "$0")/.."
pkg=dev.penroam.app
ext=/sdcard/Android/data/$pkg/files
out=benchmarks/results/boox/app-check
mkdir -p "$out"
[ $# -ge 1 ] && adb install -r "$1"
adb shell dumpsys window | grep -q 'isKeyguardShowing=true' && { echo "unlock the BOOX first"; exit 1; }
( while :; do adb shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1; sleep 8; done ) & keepalive=$!
trap 'kill $keepalive 2>/dev/null' EXIT
adb shell am force-stop $pkg
printf boox | adb shell "cat > $ext/autobench.txt"
adb logcat -c
adb shell am start -n $pkg/.MainActivity >/dev/null
wait_log() { for _ in $(seq 1 "$2"); do adb logcat -d | grep -q "$1" && return 0; sleep 1; done; echo "timeout waiting for $1"; return 1; }
wait_log 'phaseA_nativeVisible_at' 90; sleep 3
adb exec-out screencap -p > "$out/check-A-native-layer.png"
wait_log 'phaseB_webVisible_at' 30; sleep 3
adb exec-out screencap -p > "$out/check-B-web-layer.png"
wait_log 'boox-check done' 600
f=$(adb shell "ls -t $ext/perf | grep app-boox-check | head -1" | tr -d '\r')
adb pull "$ext/perf/$f" "$out/" >/dev/null
adb logcat -d | grep -E 'penroam\[' > "$out/check-log.txt" || true
echo "results: $out/$f, screenshots check-A/B in $out (compare stroke position)"
cat <<'MANUAL'

Manual part (needs the pen):
 1. Open the "BOOX check" notebook, pen tool: write a few words. Ink must appear
    immediately (hardware preview), stay after pen-up, never blink or double.
 2. Eraser tool + pen eraser button: strokes vanish as you cross them.
 3. Switch to Text, type with the keyboard; switch back to Pen: ink, text and
    template stay aligned (check all four page corners after zooming/panning).
 4. Rotate the device, open/close the page drawer, background and resume the app:
    no stale overlay, no ghost strokes, ink still aligned.
 5. Two-OS transfer:
      adb push tests/fixtures/linux-export.penroam /sdcard/Download/
    Library > Import .penroam > Download/linux-export.penroam; open it, add a
    stroke, Export .penroam to Download/boox-export.penroam, then
      adb pull /sdcard/Download/boox-export.penroam /tmp/
      python3 tools/compare-penroam.py tests/fixtures/linux-export.penroam /tmp/boox-export.penroam
    Only the page you edited may differ (one extra stroke).
MANUAL
