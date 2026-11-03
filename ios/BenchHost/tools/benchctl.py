#!/usr/bin/env python3
"""Drive the iPad harness over USB (usbmuxd tunnel to the app's 127.0.0.1:8765).

    tools/benchctl.py status
    tools/benchctl.py cmd bench-native|bench-web|export|engine-native|engine-web|clear|reset
    tools/benchctl.py pull [out_dir]          # copies perf/*.json and strokes/* not already present
    tools/benchctl.py import <strokes.json>   # load a canonical stroke document into the PencilKit engine

No libusbmuxd tools needed: speaks the usbmuxd plist protocol directly.
"""
import json, os, plistlib, socket, struct, sys

PORT = 8765


def mux_request(sock, msg, tag=1):
    payload = plistlib.dumps(msg)
    sock.sendall(struct.pack("<IIII", 16 + len(payload), 1, 8, tag) + payload)
    hdr = b""
    while len(hdr) < 16:
        hdr += sock.recv(16 - len(hdr))
    length = struct.unpack("<IIII", hdr)[0]
    body = b""
    while len(body) < length - 16:
        body += sock.recv(length - 16 - len(body))
    return plistlib.loads(body)


def base(mtype):
    return {"MessageType": mtype, "ClientVersionString": "benchctl", "ProgName": "benchctl"}


def connect():
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect("/var/run/usbmuxd")
    devs = mux_request(s, base("ListDevices"))["DeviceList"]
    usb = [d for d in devs if d["Properties"].get("ConnectionType") == "USB"] or devs
    if not usb:
        sys.exit("no device")
    dev_id = usb[0]["DeviceID"]
    s.close()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect("/var/run/usbmuxd")
    msg = base("Connect")
    msg.update({"DeviceID": dev_id, "PortNumber": socket.htons(PORT)})
    r = mux_request(s, msg)
    if r.get("Number", 1) != 0:
        sys.exit(f"usbmux connect failed: {r} (is the app open in the foreground?)")
    return s


def http(method, path, body=b""):
    s = connect()
    s.settimeout(60)
    s.sendall(f"{method} {path} HTTP/1.1\r\nHost: ipad\r\nContent-Length: {len(body)}\r\n\r\n".encode() + body)
    data = b""
    while True:
        chunk = s.recv(65536)
        if not chunk:
            break
        data += chunk
    s.close()
    head, _, body = data.partition(b"\r\n\r\n")
    code = int(head.split(b" ")[1])
    return code, body


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    op = sys.argv[1]
    if op == "status":
        code, body = http("GET", "/status")
        print(body.decode())
    elif op == "cmd":
        code, body = http("POST", f"/cmd?c={sys.argv[2]}")
        print(code, body.decode())
    elif op == "pull":
        out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(__file__), "../../../benchmarks/results/ipad")
        os.makedirs(out, exist_ok=True)
        _, body = http("GET", "/files")
        listing = json.loads(body)
        for d in ("perf", "strokes"):
            for name in listing.get(d, []):
                dest = os.path.join(out, name)
                if os.path.exists(dest):
                    continue
                code, data = http("GET", f"/file?name={d}/{name}")
                if code == 200:
                    open(dest, "wb").write(data)
                    print(f"pulled {d}/{name} ({len(data)} bytes)")
    elif op == "import":
        code, body = http("POST", "/cmd?c=pencil-import", open(sys.argv[2], "rb").read())
        print(code, body.decode())
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
