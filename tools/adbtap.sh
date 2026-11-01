#!/usr/bin/env bash
# Tap a UI element on the connected Android device by its exact text or
# content-desc, using a fresh uiautomator dump (positions change with layout).
#   tools/adbtap.sh "Export perf"
set -euo pipefail
export PATH="$HOME/Android/sdk/platform-tools:$PATH"
label="$1"
adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1
xml=$(adb shell cat /sdcard/ui.xml)
b=$(python3 -c '
import re,sys
x,l=sys.stdin.read(),sys.argv[1]
for m in re.finditer(r"<node [^>]*>",x):
    n=m.group(0)
    t=re.search(r"text=\"([^\"]*)\"",n).group(1); d=re.search(r"content-desc=\"([^\"]*)\"",n).group(1)
    if l in (t,d):
        print(*map(int,re.findall(r"\d+",re.search(r"bounds=\"([^\"]*)\"",n).group(1)))); break
' "$label" <<<"$xml")
[ -n "$b" ] || { echo "not found: $label" >&2; exit 1; }
read x1 y1 x2 y2 <<<"$b"
adb shell input tap $(( (x1+x2)/2 )) $(( (y1+y2)/2 ))
echo "tapped '$label' at $(( (x1+x2)/2 )),$(( (y1+y2)/2 ))"
