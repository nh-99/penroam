#!/usr/bin/env node
// Captures the Google Play store screenshots: a phone, a 7-inch tablet and a 10-inch tablet.
//
// For each device it boots an Android emulator made for the job, installs the app, puts the
// demo library from capture.sh in its data folder and walks through the features below,
// saving one PNG per feature. The shots are taken with `screencap`, so they are exactly what
// the screen shows, status bar and all (the status bar is held in SystemUI demo mode: a fixed
// clock, full battery and signal, no notifications).
//
// The desktop scripts replay recorded pixel coordinates. That can't work across three screen
// sizes, so this one drives the app through the page itself: it finds controls by their
// accessible names over the Chrome DevTools protocol and waits for the page to settle rather
// than sleeping for fixed times. A renamed button breaks the run with a message naming it.
//
// Every display is exactly 16:9 or 9:16, the ratio Play asks for:
//
//   phone      1080 x 1920 at 420 dpi, portrait    411 x 731 dp
//   tablet7    1080 x 1920 at 320 dpi, portrait    600 x 1067 dp, 6.9"
//   tablet10   2560 x 1440 at 280 dpi, landscape  1463 x 823 dp, 10.5"
//
// Output goes to target/play-screenshots/{phoneScreenshots,sevenInchScreenshots,
// tenInchScreenshots}/, the folder names `fastlane supply` reads. Nothing in the repository
// changes. A device's folder is replaced only when every one of its shots was taken.
//
// The emulators (AVDs named penroam-shots-*) are created on first use from a Google APIs
// system image, which, unlike a Play Store image, lets adb run as root; root is what puts
// the demo library into the app's private folder. Each run wipes their data, so every run
// starts from a fresh install. The app is a release build with the WebView inspector
// compiled in (`--features tauri/devtools`), signed with the debug key; that APK is for
// screenshots only and must not be uploaded.
//
// Needs: the Android SDK (ANDROID_HOME) with the emulator, platform tools, command-line
// tools and the system image below, hardware acceleration for the emulator, Node 22 or
// later, Python 3 and the normal Android build toolchain (docs/development.md).
//
//   node tools/screenshots/capture-android.mjs                    # all three devices
//   node tools/screenshots/capture-android.mjs phone tablet10     # some of them
//   node tools/screenshots/capture-android.mjs --show             # with emulator windows
//
// Environment: SKIP_BUILD=1 reuses the last APK; PENROAM_APK=path installs that APK instead
// (it must have the inspector compiled in); PENROAM_AVD_IMAGE overrides the system image;
// PLAY_SHOTS_OUT overrides the output folder; PYTHON names the Python 3 interpreter.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { arch, homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { DevTools } from "./devtools.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const APP_DIR = join(ROOT, "apps/desktop-mobile");
const APK_DIR = join(APP_DIR, "src-tauri/gen/android/app/build/outputs/apk");
const OUT = resolve(process.env.PLAY_SHOTS_OUT ?? join(ROOT, "target/play-screenshots"));
const PACKAGE = "ink.penroam.app";
const ACTIVITY = `${PACKAGE}/.MainActivity`;
/** Tauri's app data folder on Android is the package's data folder; the library sits in it. */
const LIBRARY = `/data/data/${PACKAGE}/library`;
const WIN = process.platform === "win32";

const ARM = arch() === "arm64";
const RUST_TARGET = ARM ? "aarch64" : "x86_64";
const IMAGE = process.env.PENROAM_AVD_IMAGE ?? `system-images;android-36;google_apis;${ARM ? "arm64-v8a" : "x86_64"}`;

/**
 * The three store listings. `port` is the emulator's console port (its adb serial is
 * emulator-PORT); `devtools` is the host port the webview's inspector is forwarded to.
 */
const DEVICES = {
  phone: { folder: "phoneScreenshots", width: 1080, height: 1920, density: 420, orientation: "portrait", port: 5584, devtools: 9341 },
  tablet7: { folder: "sevenInchScreenshots", width: 1080, height: 1920, density: 320, orientation: "portrait", port: 5586, devtools: 9342 },
  tablet10: { folder: "tenInchScreenshots", width: 2560, height: 1440, density: 280, orientation: "landscape", port: 5588, devtools: 9343 },
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (msg) => console.log(msg);

// ------------------------------------------------------------------ tools --

const SDK = process.env.ANDROID_HOME ?? process.env.ANDROID_SDK_ROOT;
const sdkTool = (rel, exe) => join(SDK ?? "", rel, WIN ? `${exe}${exe === "avdmanager" ? ".bat" : ".exe"}` : exe);

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: "inherit", cwd: ROOT, shell: WIN, ...opts });
  if (r.status !== 0) throw new Error(`${cmd} ${args.join(" ")} failed (${r.status ?? r.error})`);
}

/** Runs adb against one emulator and returns its standard output. */
function adb(serial, args, { input, allowFail = false, binary = false } = {}) {
  const r = spawnSync(sdkTool("platform-tools", "adb"), ["-s", serial, ...args], {
    input,
    maxBuffer: 256 * 1024 * 1024,
    encoding: binary ? "buffer" : "utf8",
  });
  if (r.status !== 0 && !allowFail) {
    throw new Error(`adb ${args.join(" ")} failed: ${String(r.stderr || r.stdout || r.error).trim()}`);
  }
  return binary ? r.stdout : (r.stdout ?? "").trim();
}

const shell = (serial, cmd, opts) => adb(serial, ["shell", cmd], opts);

async function waitUntil(what, test, timeoutMs, everyMs = 500) {
  const until = Date.now() + timeoutMs;
  for (;;) {
    let last;
    try {
      if (await test()) return;
    } catch (e) {
      last = e;
    }
    if (Date.now() > until) throw new Error(`timed out waiting for ${what}${last ? ` (${last.message})` : ""}`);
    await sleep(everyMs);
  }
}

// ---------------------------------------------------------------- build --

/** The newest release APK under the Gradle outputs. */
function findApk() {
  const found = [];
  const walk = (d) => {
    for (const e of existsSync(d) ? readdirSync(d, { withFileTypes: true }) : []) {
      const p = join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (/release\.apk$/.test(e.name)) found.push(p);
    }
  };
  walk(APK_DIR);
  found.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  return found[0];
}

function apk() {
  if (process.env.PENROAM_APK) return resolve(process.env.PENROAM_APK);
  if (!process.env.SKIP_BUILD || !findApk()) {
    // the inspector is how the script drives the app; release builds leave it out otherwise
    run("pnpm", ["tauri", "android", "build", "--apk", "--target", RUST_TARGET, "--features", "tauri/devtools"], { cwd: APP_DIR });
  }
  const p = findApk();
  if (!p) throw new Error(`no release APK under ${APK_DIR}`);
  return p;
}

function demoLibrary(work) {
  const pdf = join(work, "reading.pdf");
  const library = join(work, "library");
  run(process.env.PYTHON ?? (WIN ? "python" : "python3"), ["-I", "tools/screenshots/make_pdf.py", pdf]);
  run("cargo", ["run", "-q", "-p", "penroam-storage", "--example", "demo_library", "--", library, pdf]);
  return library;
}

// --------------------------------------------------------------- emulator --

const avdName = (id) => `penroam-shots-${id}`;
const avdHome = () => process.env.ANDROID_AVD_HOME ?? join(process.env.ANDROID_USER_HOME ?? join(homedir(), ".android"), "avd");

/** Creates the device's AVD if it is missing, then (re)writes its display settings. */
function ensureAvd(id, d) {
  const name = avdName(id);
  const config = join(avdHome(), `${name}.avd`, "config.ini");
  if (!existsSync(config)) {
    log(`creating emulator ${name}`);
    const r = spawnSync(sdkTool("cmdline-tools/latest/bin", "avdmanager"), ["create", "avd", "-n", name, "-k", WIN ? `"${IMAGE}"` : IMAGE, "--force"], {
      input: "no\n", // "Do you wish to create a custom hardware profile?"
      encoding: "utf8",
      shell: WIN,
    });
    if (r.status !== 0 || !existsSync(config)) {
      throw new Error(`could not create ${name}: ${(r.stderr || r.stdout || "").trim()}\n` + `Install the system image with: sdkmanager "${IMAGE}"`);
    }
  }
  const settings = {
    "hw.lcd.width": d.width,
    "hw.lcd.height": d.height,
    "hw.lcd.density": d.density,
    "hw.initialOrientation": d.orientation,
    "skin.name": `${d.width}x${d.height}`,
    "skin.path": "_no_skin",
    "showDeviceFrame": "no",
    // a hardware keyboard keeps the on-screen keyboard from covering the search shot
    "hw.keyboard": "yes",
    "hw.ramSize": 4096,
    "disk.dataPartition.size": "6G",
    "hw.gpu.enabled": "yes",
  };
  const lines = readFileSync(config, "utf8").split(/\r?\n/).filter((l) => l && !(l.split("=")[0].trim() in settings));
  for (const [k, v] of Object.entries(settings)) lines.push(`${k}=${v}`);
  writeFileSync(config, lines.join("\n") + "\n");
}

class Emulator {
  constructor(id, d, show) {
    this.id = id;
    this.d = d;
    this.serial = `emulator-${d.port}`;
    this.show = show;
    this.proc = null;
  }

  async boot() {
    if (adb(this.serial, ["get-state"], { allowFail: true }) === "device") {
      throw new Error(`${this.serial} is already running; close it first (this run wipes its data)`);
    }
    log(`booting ${avdName(this.id)}`);
    const args = ["-avd", avdName(this.id), "-port", String(this.d.port), "-wipe-data", "-no-snapshot", "-no-boot-anim", "-no-audio"];
    if (!this.show) args.push("-no-window", "-gpu", "swiftshader_indirect");
    this.proc = spawn(sdkTool("emulator", "emulator"), args, { stdio: "ignore" });
    this.exited = new Promise((r) => this.proc.once("exit", r));
    this.proc.once("exit", (code) => {
      if (!this.stopping) console.error(`the ${this.id} emulator exited (${code})`);
    });
    await waitUntil("the emulator to boot", () => shell(this.serial, "getprop sys.boot_completed", { allowFail: true }) === "1", 300000, 2000);
    adb(this.serial, ["root"]);
    await waitUntil("adb to restart as root", () => shell(this.serial, "id -u", { allowFail: true }) === "0", 60000, 1000);
    await waitUntil("the package manager", () => shell(this.serial, "pm path android", { allowFail: true }).startsWith("package:"), 60000, 1000);
  }

  /** Quiet, repeatable screen: no animations, fixed rotation, a light theme, a tidy status bar. */
  async prepare() {
    const s = this.serial;
    for (const k of ["window_animation_scale", "transition_animation_scale", "animator_duration_scale"]) shell(s, `settings put global ${k} 0`);
    shell(s, "settings put system accelerometer_rotation 0");
    shell(s, "settings put system user_rotation 0");
    shell(s, "settings put secure show_ime_with_hard_keyboard 0");
    shell(s, "cmd uimode night no");
    // Shortly after boot the system resets this setting (more than once, at no fixed time),
    // and SystemUI ignores demo commands while it is off. So keep writing it until it has
    // held for a while; shot() also checks it again before every capture.
    let heldSince = 0;
    await waitUntil(
      "status bar demo mode to stay allowed",
      () => {
        if (shell(s, "settings get global sysui_demo_allowed") !== "1") {
          shell(s, "settings put global sysui_demo_allowed 1");
          heldSince = 0;
          return false;
        }
        heldSince ||= Date.now();
        return Date.now() - heldSince >= 10000;
      },
      120000,
      1000,
    );
    this.demoStatusBar();
  }

  /** Puts the status bar in SystemUI demo mode: fixed clock, full battery and signal, no notifications. */
  demoStatusBar() {
    const s = this.serial;
    shell(s, "settings put global sysui_demo_allowed 1");
    const demo = (cmd, extras = "") => shell(s, `am broadcast -a com.android.systemui.demo -e command ${cmd} ${extras}`);
    demo("enter");
    demo("clock", "-e hhmm 0941");
    demo("battery", "-e level 100 -e plugged false -e powersave false");
    demo("network", "-e wifi show -e level 4 -e fully true");
    demo("network", "-e mobile hide");
    demo("notifications", "-e visible false");
  }

  /** Re-enters demo mode if the system has switched it off since; waits for the bar to redraw. */
  async keepDemoStatusBar() {
    if (shell(this.serial, "settings get global sysui_demo_allowed") === "1") return;
    log(`  (${this.id}: status bar demo mode was reset; entering it again)`);
    this.demoStatusBar();
    await sleep(1000);
  }

  install(apkPath) {
    log(`installing ${apkPath}`);
    adb(this.serial, ["install", "-r", "-g", apkPath]);
  }

  /** Replaces the app's library with the demo library (the app must not be running). */
  pushLibrary(library) {
    const s = this.serial;
    shell(s, `am force-stop ${PACKAGE}`);
    shell(s, "rm -rf /data/local/tmp/penroam-library");
    adb(s, ["push", library, "/data/local/tmp/penroam-library"]);
    const uid = shell(s, `stat -c %u /data/data/${PACKAGE}`);
    if (!/^\d+$/.test(uid)) throw new Error(`could not read the app's user id (${uid})`);
    shell(s, `rm -rf ${LIBRARY} && cp -r /data/local/tmp/penroam-library ${LIBRARY} && chown -R ${uid}:${uid} ${LIBRARY} && restorecon -RF ${LIBRARY}`);
    shell(s, "rm -rf /data/local/tmp/penroam-library");
  }

  /** Starts the app (stopping it first) and connects to its webview's inspector. */
  async launch() {
    const s = this.serial;
    shell(s, `am force-stop ${PACKAGE}`);
    adb(s, ["forward", "--remove", `tcp:${this.d.devtools}`], { allowFail: true });
    shell(s, `am start -W -n ${ACTIVITY}`);
    let pid = "";
    await waitUntil("the app to start", () => (pid = shell(s, `pidof ${PACKAGE}`, { allowFail: true })) !== "", 30000);
    const socket = `webview_devtools_remote_${pid}`;
    await waitUntil(
      "the webview inspector (is the APK built with --features tauri/devtools?)",
      () => shell(s, "cat /proc/net/unix", { allowFail: true }).includes(`@${socket}`),
      30000,
    );
    adb(s, ["forward", `tcp:${this.d.devtools}`, `localabstract:${socket}`]);
    return new Page(await DevTools.connect(this.d.devtools), this);
  }

  /** The screen as PNG, checked against the display size. */
  screencap() {
    const png = adb(this.serial, ["exec-out", "screencap", "-p"], { binary: true });
    if (png.length < 24 || png.readUInt32BE(0) !== 0x89504e47) throw new Error("screencap did not return a PNG");
    const w = png.readUInt32BE(16), h = png.readUInt32BE(20);
    if (w !== this.d.width || h !== this.d.height) {
      throw new Error(`the ${this.id} screen is ${w}x${h}, not ${this.d.width}x${this.d.height}; did the emulator rotate?`);
    }
    return png;
  }

  async stop() {
    if (!this.proc) return;
    this.stopping = true;
    adb(this.serial, ["emu", "kill"], { allowFail: true });
    const done = await Promise.race([this.exited.then(() => true), sleep(20000).then(() => false)]);
    if (!done) this.proc.kill();
  }
}

// ------------------------------------------------------------------- page --

/** The app's page, driven by accessible names. */
class Page {
  constructor(dt, emu) {
    this.dt = dt;
    this.emu = emu;
  }

  /** Waits for the page to be idle: nothing loading and two quiet animation frames. */
  async settle(extraMs = 800) {
    await waitUntil("the page to finish loading", () => this.dt.eval(`!document.querySelector("main .animate-spin, [role=dialog] .animate-spin")`), 60000, 250);
    await this.dt.eval("new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r(true))))");
    await sleep(extraMs);
  }

  /**
   * Clicks the first visible element matching `selector`, optionally the one whose text
   * includes `text`. Clicking through the DOM rather than with a synthetic pointer keeps
   * hover and focus styles out of the shots.
   */
  async click(selector, text, timeoutMs = 20000) {
    const expr = `(() => {
      const els = [...document.querySelectorAll(${JSON.stringify(selector)})]
        .filter((e) => e.getClientRects().length > 0 && !e.disabled)
        .filter((e) => ${JSON.stringify(text ?? null)} === null || e.textContent.includes(${JSON.stringify(text ?? "")}));
      if (!els.length) return false;
      els[0].scrollIntoView({ block: "nearest" });
      els[0].click();
      return true;
    })()`;
    await waitUntil(`${selector}${text ? ` with "${text}"` : ""}`, () => this.dt.eval(expr), timeoutMs, 250);
  }

  async key(key, code, vk, ctrl = false) {
    const base = { key, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk, modifiers: ctrl ? 2 : 0 };
    await this.dt.send("Input.dispatchKeyEvent", { type: "rawKeyDown", ...base });
    await this.dt.send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
    await sleep(50);
  }

  async waitFor(what, expr, timeoutMs = 30000) {
    await waitUntil(what, () => this.dt.eval(expr), timeoutMs, 250);
  }

  // --- the app's own steps ---

  async libraryLoaded() {
    await this.waitFor("the demo library", `document.querySelectorAll('button[aria-label^="Open "]').length >= 6`);
    await this.settle();
  }

  async open(title) {
    await this.click(`button[aria-label="Open ${title}"]`);
    await this.waitFor(`"${title}" to open`, `!!document.querySelector('[aria-label="Page canvas"] canvas')`);
    await this.settle(1500);
  }

  async back() {
    await this.click('button[aria-label="Back to notebooks"]');
    await this.libraryLoaded();
  }

  async nextPage(times = 1) {
    for (let i = 0; i < times; i++) {
      await this.click('button[aria-label="Next page"]');
      await this.settle(600);
    }
  }

  /** Zooms the open page out by `steps` (each step is the editor's Ctrl+minus, 80%). */
  async zoomOut(steps) {
    for (let i = 0; i < steps; i++) await this.key("-", "Minus", 189, true);
    await this.settle(1200);
  }

  async setTheme(label) {
    await this.click('button[aria-label="Settings"]');
    await this.click('[role="radiogroup"][aria-label="Theme"] [role="radio"]', label);
    await this.click('[role="dialog"] button[aria-label="Close"]');
    await this.settle(1200);
  }

  async search(query) {
    await this.click('button[aria-label="Search notebooks"]');
    await this.waitFor("the search box", `document.activeElement?.type === "search"`, 10000);
    await this.dt.send("Input.insertText", { text: query });
    await this.key("Enter", "Enter", 13);
    // the index is built on first start; wait until it has caught up and results are in
    await this.waitFor(
      "search results",
      `document.querySelectorAll('[role=dialog] mark').length > 0 && !document.querySelector('[role=dialog]')?.textContent.includes("Results may be incomplete")`,
      120000,
    );
    await this.settle(1000);
  }

  async shot(dir, name) {
    // the caret blinks; take it out of the shot
    await this.dt.eval(`document.activeElement?.blur?.(), true`).catch(() => {});
    await this.emu.keepDemoStatusBar();
    writeFileSync(join(dir, `${name}.png`), this.emu.screencap());
    log(`  ${this.emu.id}/${name}.png`);
  }
}

// ----------------------------------------------------------------- scenes --

/**
 * The listing, in store order: the first shots are the ones most people see.
 *
 * Pages open fitted to the width. That suits the portrait screens, but on the landscape
 * tablet it shows only the top of a page in large print, so there each page is zoomed out
 * two steps (the editor resets the zoom whenever the page changes).
 */
async function scenes(emu, page, dir) {
  const wide = emu.d.orientation === "landscape";
  const fit = () => (wide ? page.zoomOut(2) : undefined);
  // open every notebook once, so the library shows page thumbnails
  for (const t of ["Science fair ideas", "Club meeting notes", "Sketchbook", "Trip to Lisbon", "Reading: spaced repetition", "Physics: mechanics"]) {
    await page.open(t);
    await page.back();
  }

  await page.open("Physics: mechanics");
  await fit();
  await page.shot(dir, "01-handwriting");

  // the lab write-up: Markdown, a formula and a spreadsheet table
  await page.nextPage(3);
  await fit();
  await page.shot(dir, "02-typed-text-and-tables");
  await page.back();

  await page.open("Reading: spaced repetition");
  await fit();
  await page.shot(dir, "03-pdf-annotation");
  await page.back();

  await page.shot(dir, "04-library");

  // one query that finds handwriting, typed text and PDF text
  await page.search("questions");
  await page.shot(dir, "05-search");
  // the panel's open state outlives the library view, so close it rather than leave it
  await page.click('[role="dialog"] button[aria-label="Close"]');
  await page.settle();

  await page.open("Science fair ideas");
  await page.shot(dir, "06-infinite-canvas");
  await page.back();

  // dark: the system's night mode, so the status bar matches; the app follows the system
  shell(emu.serial, "cmd uimode night yes");
  page.dt.close();
  page = await emu.launch();
  await page.libraryLoaded();
  await page.open("Trip to Lisbon");
  await fit();
  await page.shot(dir, "07-dark-theme");
  await page.back();

  shell(emu.serial, "cmd uimode night no");
  page.dt.close();
  page = await emu.launch();
  await page.libraryLoaded();
  await page.setTheme("ePaper");
  await page.open("Trip to Lisbon");
  await fit();
  await page.shot(dir, "08-epaper-theme");
  page.dt.close();
}

// ------------------------------------------------------------------- main --

async function capture(id, apkPath, library, show) {
  const d = DEVICES[id];
  ensureAvd(id, d);
  const emu = new Emulator(id, d, show);
  const shots = mkdtempSync(join(tmpdir(), `penroam-play-${id}-`));
  try {
    await emu.boot();
    await emu.prepare();
    emu.install(apkPath);
    emu.pushLibrary(library);
    const page = await emu.launch();
    await page.libraryLoaded();
    await scenes(emu, page, shots);
    // all or nothing per device, so a failed run never leaves a mix of old and new shots
    const dest = join(OUT, d.folder);
    rmSync(dest, { recursive: true, force: true });
    mkdirSync(OUT, { recursive: true });
    renameSync(shots, dest);
    log(`${id}: screenshots written to ${dest}`);
  } finally {
    await emu.stop();
    rmSync(shots, { recursive: true, force: true });
  }
}

async function main() {
  const args = process.argv.slice(2);
  const show = args.includes("--show");
  const ids = args.filter((a) => !a.startsWith("--"));
  for (const id of ids) if (!DEVICES[id]) throw new Error(`unknown device ${id}; choose from ${Object.keys(DEVICES).join(", ")}`);
  if (!SDK) throw new Error("set ANDROID_HOME to the Android SDK");
  if (typeof WebSocket === "undefined") throw new Error("needs Node 22 or later (built-in WebSocket)");

  const apkPath = apk();
  const work = mkdtempSync(join(tmpdir(), "penroam-play-"));
  try {
    const library = demoLibrary(work);
    for (const id of ids.length ? ids : Object.keys(DEVICES)) await capture(id, apkPath, library, show);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error(e.message ?? e);
  process.exit(1);
});
