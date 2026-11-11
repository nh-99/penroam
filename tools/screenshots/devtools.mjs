// A minimal Chrome DevTools protocol client, shared by the screenshot scripts.
//
// capture-windows.mjs reaches the app's WebView2 on a loopback port it asks the webview to
// open; capture-android.mjs reaches the Android WebView through an `adb forward` of the
// webview's DevTools socket. Both then talk to the page the same way: one WebSocket, one
// request id per command, responses matched back by id. Events are ignored.
//
// Needs Node 22 or later (for the built-in WebSocket).

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** One DevTools connection to the app's page. */
export class DevTools {
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
    // a dropped connection (the app crashed or was stopped) fails every pending command
    ws.addEventListener("close", () => {
      for (const w of this.waiting.values()) w.reject(new Error(`${w.method}: the DevTools connection closed`));
      this.waiting.clear();
    });
  }

  /**
   * Connects to the first page target listed on `127.0.0.1:port`, retrying for about
   * `timeoutMs` while the webview starts up.
   */
  static async connect(port, timeoutMs = 30000) {
    const until = Date.now() + timeoutMs;
    while (Date.now() < until) {
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
    throw new Error(`the app's webview did not answer on DevTools port ${port}`);
  }

  /** Sends one protocol command and resolves with its result. */
  send(method, params = {}) {
    const id = ++this.next;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => this.waiting.set(id, { resolve, reject, method }));
  }

  /** Evaluates `expression` in the page and returns its (JSON) value; page exceptions throw. */
  async eval(expression) {
    const r = await this.send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(`in the page: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
    return r.result.value;
  }

  close() {
    this.ws.close();
  }
}
