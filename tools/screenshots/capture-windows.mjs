#!/usr/bin/env node
// Regenerates the README and user manual screenshots in docs/images/ on Windows.
//
// The Windows counterpart of capture.sh. It builds the same demo library, opens it in the
// release app and replays the same session.txt. Instead of an X display and XTest, it drives
// the app's WebView2 over the Chrome DevTools protocol: input goes in as DevTools mouse and
// key events, and shots come from the page itself. The viewport is held at the 1280x900
// window layout and rendered at 1.25x, so shots come out 1600 px wide with no resizing.
//
// The app runs on a fresh library with its own WebView2 profile (the session changes the
// theme, which lives in the webview's localStorage), so the developer's own install is not
// touched. The only thing it writes outside its temporary folder is the demo library's
// search index in the app's cache folder, and it removes that afterwards.
//
// Needs: Windows with the WebView2 runtime, Node 22 or later (for the built-in WebSocket),
// Python 3, and the normal app build toolchain.
//
//   node tools/screenshots/capture-windows.mjs              # build the app if missing, capture
//   set SKIP_BUILD=1 & node tools/screenshots/capture-windows.mjs
//
// Shots are rendered by Chromium on Windows rather than WebKitGTK, so text and anti-aliasing
// differ slightly from the Linux captures.
import { spawn, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdtempSync, mkdirSync, openSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const OUT = join(ROOT, "docs/images");
const BIN = process.env.PENROAM_BIN ?? join(ROOT, "target/release/penroam-app.exe");
const SESSION = join(ROOT, "tools/screenshots/session.txt");
const PORT = Number(process.env.CAPTURE_DEVTOOLS_PORT ?? 9333);
/** Logical window layout the session was recorded for. */
const VIEW = { width: 1280, height: 900 };
/** session.txt coordinates are pixels at twice the logical layout (GDK_SCALE=2). */
const SESSION_SCALE = 2;
/** 1280 × 1.25 = the 1600 px the README images are stored at. */
const SHOT_SCALE = 1.25;
const SETTLE_MS = Number(process.env.E2E_SETTLE ?? 6) * 1000;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: "inherit", cwd: ROOT, shell: process.platform === "win32", ...opts });
  if (r.status !== 0) throw new Error(`${cmd} ${args.join(" ")} failed (${r.status ?? r.error})`);
}

// --------------------------------------------------------------- DevTools --

/** A minimal Chrome DevTools protocol client over the built-in WebSocket. */
class DevTools {
  constructor(ws) {
    this.ws = ws;
    this.next = 0;
    this.waiting = new Map();
    ws.addEventListener("message", (m) => {
      const msg = JSON.parse(m.data);
      const w = this.waiting.get(msg.id);
      if (!w) return;
      this.waiting.delete(msg.id);
      if (msg.error) w.reject(new Error(`${w.method}: ${msg.error.message}`));
      else w.resolve(msg.result);
    });
  }

  static async connect(port) {
    for (let i = 0; i < 150; i++) {
      try {
        const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
        const page = targets.find((t) => t.type === "page" && !t.url.startsWith("devtools:"));
        if (page) {
          const ws = new WebSocket(page.webSocketDebuggerUrl);
          await new Promise((res, rej) => {
            ws.addEventListener("open", res, { once: true });
            ws.addEventListener("error", () => rej(new Error("could not connect to the webview")), { once: true });
          });
          return new DevTools(ws);
        }
      } catch {
        /* not listening yet */
      }
      await sleep(200);
    }
    throw new Error(`the app's webview did not open a DevTools port on ${port}`);
  }

  send(method, params = {}) {
    const id = ++this.next;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => this.waiting.set(id, { resolve, reject, method }));
  }

  close() {
    this.ws.close();
  }
}

// ------------------------------------------------------------------ input --

const BUTTONS = { 1: ["left", 1], 2: ["middle", 4], 3: ["right", 2] };
/** xdrive key names (X keysyms) as DevTools key, code and Windows virtual key code. */
const NAMED_KEYS = {
  Return: ["Enter", "Enter", 13],
  Escape: ["Escape", "Escape", 27],
  Tab: ["Tab", "Tab", 9],
  BackSpace: ["Backspace", "Backspace", 8],
  Delete: ["Delete", "Delete", 46],
  space: [" ", "Space", 32],
  minus: ["-", "Minus", 189],
  equal: ["=", "Equal", 187],
  plus: ["+", "Equal", 187],
  Prior: ["PageUp", "PageUp", 33],
  Next: ["PageDown", "PageDown", 34],
  Home: ["Home", "Home", 36],
  End: ["End", "End", 35],
  Left: ["ArrowLeft", "ArrowLeft", 37],
  Up: ["ArrowUp", "ArrowUp", 38],
  Right: ["ArrowRight", "ArrowRight", 39],
  Down: ["ArrowDown", "ArrowDown", 40],
};

function keyInfo(name, shift) {
  if (NAMED_KEYS[name]) return NAMED_KEYS[name];
  if (/^[a-zA-Z]$/.test(name)) {
    const up = name.toUpperCase();
    return [shift ? up : name.toLowerCase(), `Key${up}`, up.charCodeAt(0)];
  }
  if (/^[0-9]$/.test(name)) return [name, `Digit${name}`, name.charCodeAt(0)];
  throw new Error(`unknown key ${name}`);
}

/** Replays session.txt commands (the xdrive set, plus shot and quit) through DevTools. */
class Driver {
  constructor(dt, app, shots) {
    this.dt = dt;
    this.app = app;
    this.shots = shots;
    this.x = 0;
    this.y = 0;
    this.buttons = 0;
  }

  mouse(type, extra = {}) {
    return this.dt.send("Input.dispatchMouseEvent", { type, x: this.x, y: this.y, buttons: this.buttons, ...extra });
  }

  async move(x, y) {
    this.x = x / SESSION_SCALE;
    this.y = y / SESSION_SCALE;
    await this.mouse("mouseMoved");
  }

  async button(b, down) {
    if (b === 4 || b === 5) {
      // X wheel buttons: one notch per press
      if (down) await this.mouse("mouseWheel", { deltaX: 0, deltaY: b === 4 ? -100 : 100 });
      return;
    }
    const [button, bit] = BUTTONS[b] ?? BUTTONS[1];
    this.buttons = down ? this.buttons | bit : this.buttons & ~bit;
    await this.mouse(down ? "mousePressed" : "mouseReleased", { button, clickCount: 1 });
  }

  async key(spec) {
    let ctrl = false, shift = false, k = spec;
    for (;;) {
      if (k.startsWith("C-")) ctrl = true;
      else if (k.startsWith("S-")) shift = true;
      else break;
      k = k.slice(2);
    }
    const [key, code, vk] = keyInfo(k, shift);
    const modifiers = (ctrl ? 2 : 0) | (shift ? 8 : 0);
    const printable = key.length === 1 && !ctrl;
    const base = { key, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk, modifiers };
    await this.dt.send("Input.dispatchKeyEvent", { type: printable ? "keyDown" : "rawKeyDown", ...base, ...(printable ? { text: key } : {}) });
    await this.dt.send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
    await sleep(15);
  }

  async type(text) {
    // as in xdrive, a backslash types Return
    for (const ch of text) {
      if (ch === "\\") await this.key("Return");
      else if (/^[a-zA-Z0-9]$/.test(ch)) await this.key(/[A-Z]/.test(ch) ? `S-${ch}` : ch);
      else await this.dt.send("Input.insertText", { text: ch });
    }
  }

  /** `shot NAME [X Y W H]`: the whole window, or that area in session coordinates. */
  async shot(name, area) {
    const clip = area
      ? { x: area[0] / SESSION_SCALE, y: area[1] / SESSION_SCALE, width: area[2] / SESSION_SCALE, height: area[3] / SESSION_SCALE, scale: 1 }
      : { x: 0, y: 0, width: VIEW.width, height: VIEW.height, scale: 1 };
    const { data } = await this.dt.send("Page.captureScreenshot", { format: "png", clip });
    writeFileSync(join(this.shots, `${name}.png`), Buffer.from(data, "base64"));
    console.log(`  ${name}.png`);
  }

  async line(line) {
    const [cmd, ...a] = line.split(/\s+/);
    const n = a.map(Number);
    switch (cmd) {
      case "move": return this.move(n[0], n[1]);
      case "down": return this.button(n[0], true);
      case "up": return this.button(n[0], false);
      case "drag": {
        await this.move(n[0], n[1]);
        await sleep(20);
        await this.button(1, true);
        await sleep(10);
        for (let i = 1; i <= n[4]; i++) {
          await this.move(n[0] + ((n[2] - n[0]) * i) / n[4], n[1] + ((n[3] - n[1]) * i) / n[4]);
          await sleep(5);
        }
        await this.button(1, false);
        return sleep(30);
      }
      case "key": return this.key(a[0]);
      case "type": return this.type(line.slice(5));
      case "sleep": return sleep(n[0]);
      case "shot": return this.shot(a[0], a.length >= 5 ? n.slice(1, 5) : null);
      case "quit": return this.app.quit();
      default: throw new Error(`session command not supported on Windows: ${line}`);
    }
  }
}

// -------------------------------------------------------------------- app --

class App {
  constructor(work, library) {
    this.work = work;
    this.library = library;
    this.profile = join(work, "webview");
    this.proc = null;
  }

  start() {
    const log = openSync(join(this.work, "app.log"), "a");
    this.proc = spawn(BIN, [], {
      env: {
        ...process.env,
        PENROAM_LIBRARY_DIR: this.library,
        // a profile of its own, so the session's theme changes stay out of the real install
        WEBVIEW2_USER_DATA_FOLDER: this.profile,
        // loopback only (Chromium's default for this flag)
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT}`,
      },
      stdio: ["ignore", log, log],
    });
    this.exited = new Promise((r) => this.proc.once("exit", r));
  }

  /** Polite close (WM_CLOSE, like the window's close button): the app flushes, then exits. */
  async quit() {
    spawnSync("taskkill", ["/PID", String(this.proc.pid)], { stdio: "ignore" });
    const done = await Promise.race([this.exited.then(() => true), sleep(15000).then(() => false)]);
    if (!done) throw new Error("the app did not exit within 15 s of being asked to close");
  }

  kill() {
    if (this.proc && this.proc.exitCode === null) spawnSync("taskkill", ["/F", "/PID", String(this.proc.pid)], { stdio: "ignore" });
  }
}

/** The app's search index folder; a run adds one for the demo library, removed afterwards. */
const SEARCH_CACHE = join(process.env.LOCALAPPDATA ?? "", "ink.penroam.app", "search");
const listDir = (d) => (existsSync(d) ? readdirSync(d) : []);

async function main() {
  if (process.platform !== "win32") throw new Error("this is the Windows path; use tools/screenshots/capture.sh elsewhere");
  if (typeof WebSocket === "undefined") throw new Error("needs Node 22 or later (built-in WebSocket)");
  if (!process.env.SKIP_BUILD || !existsSync(BIN)) run("pnpm", ["tauri", "build", "--no-bundle"]);

  const work = mkdtempSync(join(tmpdir(), "penroam-capture-"));
  const library = join(work, "library");
  const searchBefore = new Set(listDir(SEARCH_CACHE));
  const app = new App(work, library);
  let dt = null;
  try {
    run(process.env.PYTHON ?? "python", ["-I", "tools/screenshots/make_pdf.py", join(work, "reading.pdf")]);
    run("cargo", ["run", "-q", "-p", "penroam-storage", "--example", "demo_library", "--", library, join(work, "reading.pdf")]);

    app.start();
    dt = await DevTools.connect(PORT);
    // The WebView2 loader takes the profile folder from the environment. Check that it did
    // before anything changes a setting, so a run can never alter the real install.
    if (!existsSync(app.profile)) throw new Error("WebView2 ignored WEBVIEW2_USER_DATA_FOLDER; stopping before the session changes any setting");
    await dt.send("Emulation.setDeviceMetricsOverride", { ...VIEW, deviceScaleFactor: SHOT_SCALE, mobile: false, screenWidth: VIEW.width, screenHeight: VIEW.height });
    await sleep(SETTLE_MS);

    const session = readFileSync(SESSION, "utf8").split(/\r?\n/).map((l) => l.trim());
    // every image the session takes, in order
    const SHOTS = session.filter((l) => l.startsWith("shot ")).map((l) => l.split(/\s+/)[1]);
    const shots = join(work, "shots");
    mkdirSync(shots);
    const drive = new Driver(dt, app, shots);
    for (const line of session) {
      if (line && !line.startsWith("#")) await drive.line(line);
    }
    // all or nothing, so a failed run never leaves the README with a mix of old and new images
    const missing = SHOTS.filter((s) => !existsSync(join(shots, `${s}.png`)));
    if (missing.length) throw new Error(`session did not take: ${missing.join(", ")}`);
    mkdirSync(OUT, { recursive: true });
    for (const s of SHOTS) copyFileSync(join(shots, `${s}.png`), join(OUT, `${s}.png`));
    console.log(`screenshots written to ${OUT}`);
  } finally {
    dt?.close();
    app.kill();
    await app.exited;
    for (const d of listDir(SEARCH_CACHE)) if (!searchBefore.has(d)) rmSync(join(SEARCH_CACHE, d), { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
    // WebView2's helper processes can hold the profile for a moment after the app exits
    rmSync(work, { recursive: true, force: true, maxRetries: 10, retryDelay: 500 });
  }
}

main().catch((e) => {
  console.error(e.message ?? e);
  process.exit(1);
});
