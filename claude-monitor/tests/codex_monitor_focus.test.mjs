import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { test } from "node:test";

// Execute the production layout callback with ordinary/shared DOM dependencies, including a keyboard.
const source = await readFile(new URL("../src/codex-ui/app.js", import.meta.url), "utf8");
const match = source.match(/  const paintCompose = \(\) => \{([\s\S]*?)\n  \};\n  new MutationObserver\(paintCompose\)/);
assert(match, "Production phone layout callback discovered");
const callback = `const paintCompose = () => {${match[1]}\n}; paintCompose();`;

for (const shared of [false, true]) test(`${shared ? "shared" : "ordinary"} phone focus remains usable with an open keyboard`, () => {
  const input = { value: "Draft" }, outside = {}, pending = {}, calls = [];
  for (const active of [input, outside, pending]) {
    const scope = {
      controls: shared ? { nativeActive: true, runtimePanel: { contains: node => node === pending }, fitComposer: () => calls.push("shared-fit") } : {},
      composeInput: input, document: { activeElement: active }, innerHeight: 844,
      window: { visualViewport: { height: 508 } }, PHONE: { matches: true },
      composer: { contains: node => node === input, classList: { contains: name => name === "production-show" } },
      composeFab: { classList: { contains: () => false } },
      dockCompose: { classList: { toggle() {} }, setAttribute() {} },
      app: { classList: { toggle: (name, value) => calls.push([name, value]) } },
      byId: () => ({ textContent: "Session" }), composeNote: {}, fitCompose: () => calls.push("ordinary-fit"),
    };
    assert.doesNotThrow(() => vm.runInNewContext(callback, scope));
    assert.deepEqual(calls.splice(0), [["composing", shared ? active !== outside : true], shared ? "shared-fit" : "ordinary-fit"]);
    if (!shared) assert.equal(scope.composeNote.textContent, "To Session");
  }
});
