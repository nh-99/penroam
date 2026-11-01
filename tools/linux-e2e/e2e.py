#!/usr/bin/env python3
"""Real-input Linux E2E helper: drives the release Penroam binary under XWayland
(GDK_BACKEND=x11) with XTest mouse/keyboard events and captures window
screenshots. Coordinates in commands are relative to the window content.

    python3 tools/linux-e2e/e2e.py <script.txt> <outdir> [library-dir]

Script lines: any xdrive command (move/down/up/drag/key/type/sleep) with
window-relative coordinates, plus:
    shot NAME        save <outdir>/NAME.png
    quit             close the window (exercises flush-on-close)
    kill             SIGKILL the app (crash simulation)
    start            (re)launch the app on the same library
"""
import os, re, signal, subprocess, sys, time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "../.."))
BIN = os.environ.get("PENROAM_BIN", os.path.join(ROOT, "target/release/penroam-app"))
XDRIVE = os.environ.get("XDRIVE", os.path.join(os.path.dirname(__file__), "xdrive"))


class App:
    def __init__(self, libdir, outdir):
        self.libdir, self.outdir, self.proc, self.win, self.off = libdir, outdir, None, None, (0, 0)

    def start(self):
        env = dict(os.environ, GDK_BACKEND="x11", PENROAM_LIBRARY_DIR=self.libdir)
        self.log = open(os.path.join(self.outdir, "app.log"), "a")
        self.proc = subprocess.Popen([BIN], env=env, stdout=self.log, stderr=self.log)
        for _ in range(100):
            time.sleep(0.2)
            tree = subprocess.run(["xwininfo", "-root", "-tree"], capture_output=True, text=True).stdout
            m = re.search(r"(0x[0-9a-f]+) \"Penroam\": \(\"penroam-app\" \"Penroam-app\"\)\s+\d+x\d+\+-?\d+\+-?\d+\s+\+(-?\d+)\+(-?\d+)", tree)
            if m:
                self.win, self.off = m.group(1), (int(m.group(2)), int(m.group(3)))
                time.sleep(float(os.environ.get("E2E_SETTLE", "4")))
                return
        raise RuntimeError("Penroam window not found")

    def drive(self, cmds):
        out = []
        for c in cmds:
            p = c.split()
            if p[0] == "move":
                out.append(f"move {int(p[1]) + self.off[0]} {int(p[2]) + self.off[1]}")
            elif p[0] == "drag":
                out.append(f"drag {int(p[1]) + self.off[0]} {int(p[2]) + self.off[1]} {int(p[3]) + self.off[0]} {int(p[4]) + self.off[1]} {p[5]}")
            else:
                out.append(c)
        subprocess.run([XDRIVE], input="\n".join(out) + "\n", text=True, check=True)

    def shot(self, name):
        subprocess.run(["import", "-window", self.win, os.path.join(self.outdir, name + ".png")], check=True)

    def quit(self):
        """Polite close (WM_DELETE_WINDOW): the app flushes, then exits."""
        subprocess.run([XDRIVE], input=f"close {self.win}\n", text=True, check=True)
        self.proc.wait(timeout=15)

    def kill(self):
        self.proc.send_signal(signal.SIGKILL)
        self.proc.wait(timeout=5)


def main():
    script, outdir = sys.argv[1], sys.argv[2]
    libdir = sys.argv[3] if len(sys.argv) > 3 else os.path.join(outdir, "library")
    os.makedirs(outdir, exist_ok=True)
    app = App(libdir, outdir)
    app.start()
    batch = []
    for raw in open(script):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        word = line.split()[0]
        if word in ("shot", "kill", "start", "quit"):
            if batch:
                app.drive(batch)
                batch = []
            if word == "shot":
                app.shot(line.split()[1])
            elif word == "kill":
                app.kill()
            elif word == "quit":
                app.quit()
            else:
                app.start()
        else:
            batch.append(line)
    if batch:
        app.drive(batch)
    if app.proc and app.proc.poll() is None:
        app.kill()


if __name__ == "__main__":
    main()
