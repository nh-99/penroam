#!/usr/bin/env bash
# Builds + installs a one-shot autorun build: on next launch the app runs
# Workload D natively and in WKWebView, streaming results to the device log.
# Run tools/collect_results.py meanwhile, then open the app on the iPad.
set -euo pipefail
cd "$(dirname "$0")/.."
date +%s > Sources/M00iOS/autorun/autorun.txt
trap 'rm -f Sources/M00iOS/autorun/autorun.txt' EXIT
source ~/.local/share/swiftly/env.sh
xtool dev run || true   # install succeeds even when the debugger launch fails
