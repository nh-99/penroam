#!/usr/bin/env python3
"""Reassembles perf JSON that the iPad app streams to the device log.

    python3 ios/M00iOS/tools/collect_results.py [out_dir]   # Ctrl+C to stop

Requires libimobiledevice (`idevicesyslog`). Each export is logged as
`M00X|<name>|<i>|<n>|<base64>`; complete files are written to out_dir
(default benchmarks/results/ipad).
"""
import base64, os, re, subprocess, sys

out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "../../../benchmarks/results/ipad")
os.makedirs(out, exist_ok=True)
pat = re.compile(r"M00X\|([^|]+)\|(\d+)\|(\d+)\|([A-Za-z0-9+/=]+)")
parts = {}
proc = subprocess.Popen(["idevicesyslog", "--process", "M00iOS"], stdout=subprocess.PIPE, text=True, errors="replace")
print(f"listening… writing to {os.path.abspath(out)}")
try:
    for line in proc.stdout:
        m = pat.search(line)
        if not m:
            continue
        name, i, n, chunk = m.group(1), int(m.group(2)), int(m.group(3)), m.group(4)
        d = parts.setdefault(name, {})
        d[i] = chunk
        if len(d) == n:
            data = base64.b64decode("".join(d[k] for k in range(n)))
            with open(os.path.join(out, name), "wb") as f:
                f.write(data)
            print(f"saved {name} ({len(data)} bytes)")
            del parts[name]
except KeyboardInterrupt:
    pass
finally:
    proc.terminate()
    for name, d in parts.items():
        print(f"INCOMPLETE {name}: {len(d)} chunks received")
