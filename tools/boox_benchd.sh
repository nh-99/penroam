#!/usr/bin/env bash
# Runs Workload D (Bench D button, Mode A framework canvas) on the connected BOOX,
# alternating Tauri and Flutter, each from a fresh process. Pulls results.
#   tools/boox_benchd.sh [sessions=3]
set -uo pipefail
export PATH="$HOME/Android/sdk/platform-tools:$PATH"
cd "$(dirname "$0")/.."
N=${1:-3}
declare -A ACT=([tauri]=dev.opennote.m00.tauri/.MainActivity [flutter]=dev.opennote.m00.m00_flutter_app/.MainActivity)
declare -A PKG=([tauri]=dev.opennote.m00.tauri [flutter]=dev.opennote.m00.m00_flutter_app)
for i in $(seq 1 "$N"); do
  for app in tauri flutter; do
    pkg=${PKG[$app]}; dir=/sdcard/Android/data/$pkg/files/perf
    before=$(adb shell "ls $dir 2>/dev/null | grep -c workload-d" | tr -d '\r')
    adb shell am force-stop "$pkg"; adb shell pm enable "$pkg" >/dev/null
    adb shell am start -W -n "${ACT[$app]}" | grep -E "TotalTime" | sed "s/^/$app run $i /"
    sleep 6
    tools/adbtap.sh "Bench D" || { echo "$app run $i: Bench D not found"; continue; }
    t0=$(date +%s)
    while :; do
      now=$(adb shell "ls $dir 2>/dev/null | grep -c workload-d" | tr -d '\r')
      [ "${now:-0}" -gt "${before:-0}" ] && break
      [ $(( $(date +%s) - t0 )) -gt 900 ] && { echo "$app run $i: timeout"; break; }
      sleep 5
    done
    echo "$app run $i: done in $(( $(date +%s) - t0 )) s"
    sleep 2
  done
done
mkdir -p benchmarks/results/boox/workload-d
for app in tauri flutter; do
  pkg=${PKG[$app]}
  for f in $(adb shell "ls /sdcard/Android/data/$pkg/files/perf | grep workload-d" | tr -d '\r'); do
    adb pull "/sdcard/Android/data/$pkg/files/perf/$f" benchmarks/results/boox/workload-d/ >/dev/null
  done
done
ls benchmarks/results/boox/workload-d
