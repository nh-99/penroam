#!/usr/bin/env bash
# One benchmark session on the connected BOOX from a fresh process:
#   tools/boox_bench.sh <tauri|flutter> "<button label>" <result-file-substring> ["<mode button label>"]
# Waits for a new matching file in /sdcard/Android/data/<pkg>/files/perf, then pulls it
# into benchmarks/results/boox/sessions/.
set -uo pipefail
export PATH="$HOME/Android/sdk/platform-tools:$PATH"
cd "$(dirname "$0")/.."
app=$1; button=$2; pattern=$3; modebtn=${4:-}
case $app in
  tauri) pkg=dev.opennote.m00.tauri ;;
  flutter) pkg=dev.opennote.m00.m00_flutter_app ;;
  *) echo "unknown app $app"; exit 2 ;;
esac
dir=/sdcard/Android/data/$pkg/files/perf
count() { adb shell "ls $dir 2>/dev/null | grep -c $pattern" | tr -d '\r'; }
tap_retry() { for _ in $(seq 1 15); do tools/adbtap.sh "$1" 2>/dev/null && return 0; sleep 2; done; echo "$app: '$1' not found"; return 1; }
before=$(count)
adb shell am force-stop "$pkg"; adb shell pm enable "$pkg" >/dev/null
adb shell am start -W -n "$pkg/.MainActivity" | grep TotalTime | sed "s/^/$app /"
if [ -n "$modebtn" ]; then tap_retry "$modebtn" || exit 1; sleep 4; fi
tap_retry "$button" || exit 1
t0=$(date +%s)
while :; do
  [ "$(count)" -gt "${before:-0}" ] && break
  [ $(( $(date +%s) - t0 )) -gt 1200 ] && { echo "$app: timeout"; exit 1; }
  sleep 5
done
sleep 2
mkdir -p benchmarks/results/boox/sessions
f=$(adb shell "ls -t $dir | grep $pattern | head -1" | tr -d '\r')
adb pull "$dir/$f" benchmarks/results/boox/sessions/ >/dev/null
echo "$app '$button': $f in $(( $(date +%s) - t0 )) s"
