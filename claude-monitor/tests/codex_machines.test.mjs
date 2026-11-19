// #s595 (ab-replay's review, D1 and D2): over several machines the app mounts the session view
// again on another machine's handle when the reader opens a row there (`remountOn` in
// app-mount.js), and `backend()` then answers that machine's. Work the reader started on the first
// machine and still out across the remount finishes on the first machine: each flow takes its
// handle where the action starts and never reads `backend()` again after an await. Each module runs
// as source with its imports stripped, the names they bring given here, as the other codex_*
// tests do; `backend` answers whichever machine the test has made current.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";
import { composeCapability, consentQuery, grantOutcome, runRevoke, runSend } from "../../claude-replay-html/src/html/shared/control-protocol.js";

const controlSource = await readFile(new URL("../src/codex-ui/control-store.js", import.meta.url), "utf8");
const mdrevSource = await readFile(new URL("../src/codex-ui/mdrev-pane.js", import.meta.url), "utf8");
const strip = source => source.replace(/^import .*\n/gm, "").replace(/^export /gm, "");

/** A reply the test lets go when it chooses. */
function held() {
  let release;
  const promise = new Promise(resolve => { release = resolve; });
  return { promise, release };
}

/** One machine's handle, recording each path it is asked for; `answer(path)` gives the reply. */
function machine(name, answer) {
  const asked = [];
  const ask = (path, init) => { asked.push(path); return answer(path, init); };
  const handle = {
    base: `/${name}`, local: false, paired: true, version: null,
    mdrevVersion: () => "1.1.74", mdrevMoved() {},
    url: path => `/${name}${path}`, fetch: ask,
    mdrevUrl: path => `/${name}${path}`, mdrevFetch: ask,
  };
  return { asked, handle };
}

const reply = (body, status = 200) => ({ ok: status < 400, status, json: async () => body, text: async () => "" });

test("a send whose grant is out across a remount sends its prompt to the machine it was granted on", async () => {
  const consent = held();
  const a = machine("a", path => (path.startsWith("/api/consent") ? consent.promise : Promise.resolve(reply({ ok: true }))));
  const b = machine("b", () => Promise.resolve(reply({ ok: true })));
  let current = a.handle;
  const input = { value: "deploy now", dispatchEvent() {}, focus() {} }, sendBtn = { disabled: false };
  const context = {
    backend: () => current,
    composeCapability, consentQuery, grantOutcome, runRevoke, runSend,
    DialogCard: class {},
    viewRoot: { byId: id => (id === "composeInput" ? input : sendBtn), active: () => null },
    page: { body: () => ({ dataset: {} }) },
    Event: class { constructor(type) { this.type = type; } },
    matchMedia: () => ({ matches: false }),
  };
  const Control = vm.runInNewContext(`${strip(controlSource)}\nControlStore`, context);
  const store = Object.assign(Object.create(Control.prototype), {
    compose: { row: { id: "s1" }, mode: "tmux", consented: false, pending: "" },
    images: new Map(),
    actions: { selectedRow: () => ({ id: "s1" }), toast() {}, refreshIndex() {}, repaintComposer() {} },
    close() {},
  });
  const sending = store.send("deploy now");
  // The reader opens a row on the second machine while the first one's grant is out.
  current = b.handle;
  consent.release(reply({ ok: true }));
  await sending;
  assert.deepEqual(b.asked, [], "nothing of the send reaches the second machine");
  assert.deepEqual(a.asked.map(path => path.split("?")[0]), ["/api/consent", "/api/send"], "the grant and the prompt both go to the first machine");
});

test("a carried file whose open is out across a remount is held and mounted on the machine it came from", async () => {
  const open = held();
  const answer = path => (path.startsWith("/api/mdrev/open") ? open.promise : Promise.resolve(reply({ root: "held", path: "notes.md", cap: "c1" })));
  const a = machine("a", answer), b = machine("b", answer);
  let current = a.handle;
  const mounts = [];
  const context = {
    backend: () => current,
    assets: () => null,
    viewRoot: { theme: () => "light", themeElement: () => ({}) },
    MutationObserver: class { observe() {} disconnect() {} },
    mounts,
  };
  // The bundle is taken as loaded, as it is after the page's first document.
  const { mountMarkdown } = vm.runInNewContext(`${strip(mdrevSource)}\nloading = Promise.resolve({ mountMdrev: (el, options) => { mounts.push(options); return { unmount() {} }; } });\n({ mountMarkdown });`, context);
  const mounting = mountMarkdown({ dataset: {} }, { name: "notes.md", path: "/w/notes.md", fsig: "f1", text: "# carried" });
  current = b.handle;
  // The file has gone since the transcript carried it: the pane holds the carried text instead.
  open.release(reply({ error: "gone" }, 410));
  await mounting;
  assert.deepEqual(b.asked, [], "nothing of the first machine's file reaches the second machine");
  assert.deepEqual(a.asked.map(path => path.split("?")[0]), ["/api/mdrev/open", "/api/mdrev/hold"], "the open and the hold both go to the first machine");
  assert.equal(mounts.length, 1, "the reader mounts once");
  assert.equal(mounts[0].contract, "/a/api/mdrev", "on the first machine's contract");
});

// #s352's fallback (a page whose monitor was upgraded under it asks which mdrev the monitor serves
// now) asks no machine when the host carries its own bundle (`assets`): the host's release is the one
// it pins, and no machine's code runs in its page. The bundle's import fails here as a 404 would.
for (const [given, asks] of [[true, false], [false, true]]) {
  test(`a bundle that does not load ${asks ? "asks the server which release it serves" : "asks no machine under the host's assets"}`, async () => {
    const a = machine("a", () => Promise.resolve(reply({ root: "/w", path: "a.md", cap: "c1" })));
    const context = {
      backend: () => a.handle,
      assets: () => (given ? { mdrevUrl: path => `/app-vendor${path}`, mdrevVersion: "1.1.74", mermaid: "/app-vendor/mermaid.js" } : null),
      viewRoot: { theme: () => "light", themeElement: () => ({}), sheets: () => ({ appendChild() {} }) },
      document: { createElement: () => ({ remove() {} }) },
      MutationObserver: class { observe() {} disconnect() {} },
    };
    // No loader is given to this context, so the bundle's `import()` rejects, as a missing bundle does.
    const { mountMarkdown } = vm.runInNewContext(`${strip(mdrevSource)}\n({ mountMarkdown });`, context);
    await assert.rejects(mountMarkdown({ dataset: {} }, { name: "a.md", path: "/w/a.md", fsig: "f1" }));
    assert.equal(a.asked.some(path => path.startsWith("/markdown")), asks, a.asked.join(", "));
  });
}
