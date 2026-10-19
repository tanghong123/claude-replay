import assert from "node:assert/strict";
import test from "node:test";
import { sharedKeyboardFrame } from "../src/codex-ui/phone-keyboard.js";

test("phone controls retain their visible-bottom inset across keyboard focus scale changes", () => {
  for (const scale of [1, 1.009, 1.015, 1.1, 1.25]) {
    const height = 508 / scale;
    const frame = sharedKeyboardFrame(844, { height, scale, pageTop: 144 });
    assert.equal(frame.inset, 844 - height);
    assert.equal(frame.shift, 144);
    assert.equal(frame.keyboardOpen, true);
  }
});

test("phone controls use their actual containing block rather than the root height", () => {
  assert.deepEqual(sharedKeyboardFrame(780, { height: 444, scale: 1, pageTop: 92 }), { inset: 336, shift: 92, keyboardOpen: true });
});

test("keyboard close and user zoom preserve pan without falsely declaring a keyboard", () => {
  assert.deepEqual(sharedKeyboardFrame(844, { height: 844, scale: 1, pageTop: 144 }), { inset: 0, shift: 144, keyboardOpen: false });
  assert.deepEqual(sharedKeyboardFrame(844, { height: 422, scale: 2, pageTop: 96 }), { inset: 422, shift: 96, keyboardOpen: false });
});

test("invalid transient viewport measurements fall back without invalid CSS coordinates", () => {
  assert.deepEqual(sharedKeyboardFrame(844, { height: NaN, scale: 0, pageTop: -8 }), { inset: 0, shift: 0, keyboardOpen: false });
  assert.deepEqual(sharedKeyboardFrame(844, { height: 900, scale: Infinity, pageTop: NaN }), { inset: 0, shift: 0, keyboardOpen: false });
  for (const height of [0, -1, NaN, Infinity]) assert.throws(() => sharedKeyboardFrame(height, { height: 1, scale: 1, pageTop: 0 }), RangeError);
});
