// cdp.mjs — a dependency-free Chrome DevTools Protocol client (Node 24's native WebSocket).
// Used by the demo tour: no Playwright package is installed on this machine, and the browser
// binary from its cache plus CDP is all the tour needs.
import { spawn } from "node:child_process";
import { mkdirSync, rmSync } from "node:fs";

import { existsSync, readdirSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

/** The browser to drive: $CHROME, else the newest Playwright cache build, else a system Chrome. */
export const CHROME = (() => {
  if (process.env.CHROME && existsSync(process.env.CHROME)) return process.env.CHROME;
  const cache = join(homedir(), "Library/Caches/ms-playwright");
  if (existsSync(cache)) {
    const builds = readdirSync(cache).filter(d => d.startsWith("chromium-")).sort((a, b) => Number(b.split("-")[1]) - Number(a.split("-")[1]));
    for (const b of builds) {
      for (const rel of ["chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing", "chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing", "chrome-linux/chrome"]) {
        const p = join(cache, b, rel);
        if (existsSync(p)) return p;
      }
    }
  }
  for (const p of ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/usr/bin/google-chrome", "/usr/bin/chromium"]) if (existsSync(p)) return p;
  throw new Error("no Chrome found — set $CHROME");
})();

export async function launch({ width = 1440, height = 900, profileDir }) {
  // The profile carries the harness's own mark, so `pkill -f cr-browser-chrome-` reaps it and
  // nothing else on this machine is ever in range.
  const dir = profileDir || join(tmpdir(), `cr-browser-chrome-${process.pid}-demo`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  const args = [
    "--headless=new", `--user-data-dir=${dir}`, "--remote-debugging-port=0",
    "--no-first-run", "--no-default-browser-check", "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding",
    "--hide-scrollbars", "--force-color-profile=srgb", `--window-size=${width},${height}`,
    "about:blank",
  ];
  const child = spawn(CHROME, args, { stdio: ["ignore", "ignore", "pipe"] });
  const wsUrl = await new Promise((resolve, reject) => {
    let buf = "";
    const t = setTimeout(() => reject(new Error(`chrome did not print a devtools url:\n${buf}`)), 20000);
    child.stderr.on("data", d => {
      buf += d;
      const m = buf.match(/ws:\/\/[^\s]+/);
      if (m) { clearTimeout(t); resolve(m[0]); }
    });
  });
  return { child, wsUrl, dir };
}

export class Cdp {
  constructor(ws) { this.ws = ws; this.id = 0; this.pending = new Map(); this.handlers = new Map(); }
  static async connect(url) {
    const ws = new WebSocket(url);
    await new Promise((res, rej) => { ws.onopen = res; ws.onerror = e => rej(new Error(`ws: ${e.message || e}`)); });
    const cdp = new Cdp(ws);
    ws.onmessage = ev => {
      const msg = JSON.parse(ev.data);
      if (msg.id && cdp.pending.has(msg.id)) {
        const { resolve, reject } = cdp.pending.get(msg.id);
        cdp.pending.delete(msg.id);
        msg.error ? reject(new Error(`${msg.error.message} (${JSON.stringify(msg.error.data || "")})`)) : resolve(msg.result);
      } else if (msg.method) {
        for (const h of cdp.handlers.get(msg.method) || []) h(msg.params);
      }
    };
    return cdp;
  }
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }
  on(method, fn) { this.handlers.set(method, [...(this.handlers.get(method) || []), fn]); }
  close() { this.ws.close(); }
}

/** Attach to the first page target and return a session-bound sender. */
export async function attach(cdp) {
  const { targetInfos } = await cdp.send("Target.getTargets");
  const page = targetInfos.find(t => t.type === "page");
  const { sessionId } = await cdp.send("Target.attachToTarget", { targetId: page.targetId, flatten: true });
  const send = (method, params) => cdp.send(method, params, sessionId);
  return { sessionId, send, targetId: page.targetId };
}

export const sleep = ms => new Promise(r => setTimeout(r, ms));

/** Evaluate an expression in the page and return its JSON value. */
export async function evalIn(send, expression, awaitPromise = false) {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise });
  if (r.exceptionDetails) throw new Error(`page: ${r.exceptionDetails.text} — ${expression.slice(0, 120)}`);
  return r.result.value;
}

/** Poll a boolean expression until true, or throw with what it saw. */
export async function until(send, expression, what, timeoutMs = 30000, diag = "document.title") {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (await evalIn(send, expression) === true) return Date.now() - t0;
    await sleep(100);
  }
  throw new Error(`timed out waiting for ${what}; seen: ${JSON.stringify(await evalIn(send, diag))}`);
}
