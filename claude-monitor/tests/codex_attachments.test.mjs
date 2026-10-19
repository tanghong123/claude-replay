import assert from "node:assert/strict";
import { test } from "node:test";
import { ImageAttachmentDrafts, readImageFile, IMAGE_LIMITS } from "../src/codex-workspace/attachments.js";

const png = Uint8Array.from([137,80,78,71,13,10,26,10]);
const file = (name = "image.png", size = png.length, type = "image/png") => ({ name, size, type, arrayBuffer: async () => png.buffer });

test("file adapter accepts raster signatures and rejects mismatched MIME and declared bytes", async () => {
  assert.equal(await readImageFile(file()), "data:image/png;base64,iVBORw0KGgo=");
  await assert.rejects(readImageFile(file("wrong.jpg", 8, "image/jpeg")), /contents do not match/);
  await assert.rejects(readImageFile(file("wrong.png", 9)), /contents do not match/);
  for (const [type, bytes] of [["image/jpeg", [255,216,255]], ["image/gif", [...Buffer.from("GIF89a")]], ["image/webp", [...Buffer.from("RIFF0000WEBP")]]]) {
    assert.match(await readImageFile({ size: bytes.length, type, arrayBuffer: async () => Uint8Array.from(bytes).buffer }), new RegExp(`^data:${type};base64,`));
  }
});

test("drafts reserve count and aggregate byte limits before concurrent reads; rejected files are not read", async () => {
  let reads = 0, resolveRead;
  const hold = new Promise(resolve => { resolveRead = resolve; });
  const drafts = new ImageAttachmentDrafts(async () => { reads++; await hold; return "data:image/png;base64,iVBORw0KGgo="; });
  const adding = drafts.add("a", [file("one", IMAGE_LIMITS.fileBytes), file("two", IMAGE_LIMITS.fileBytes)]);
  assert.throws(() => drafts.snapshot("a"), /finish loading/);
  assert.match((await drafts.add("a", [file()]))[0].message, /8 MiB/); assert.equal(reads, 2);
  resolveRead(); assert.deepEqual(await adding, []); assert.equal(drafts.snapshot("a").length, 2);
  const small = new ImageAttachmentDrafts(async () => "data:image/png;base64,iVBORw0KGgo=");
  assert.deepEqual(await small.add("b", Array.from({ length: 4 }, () => file())), []);
  assert.match((await small.add("b", [file()]))[0].message, /at most 4/);
  assert.match((await small.add("c", [file("huge", IMAGE_LIMITS.fileBytes + 1)]))[0].message, /4 MiB/);
  assert.match((await small.add("c", [file("empty", 0)]))[0].message, /1 byte/);
  assert.match((await small.add("c", [file("script.svg", 8, "image/svg+xml")]))[0].message, /PNG, JPEG/);
  assert.equal(small.items("c").length, 0);
});

test("late reads stay with their captured thread; removal cancels a pending item without resurrection", async () => {
  const release = new Map();
  const drafts = new ImageAttachmentDrafts(input => new Promise(resolve => release.set(input.name, resolve)));
  const a = drafts.add("a", [file("a")]), b = drafts.add("b", [file("b")]);
  drafts.remove("a", drafts.items("a")[0].id);
  release.get("a")("data:image/png;base64,old"); release.get("b")("data:image/png;base64,current");
  await Promise.all([a,b]); assert.deepEqual(drafts.snapshot("a"), []); assert.equal(drafts.snapshot("b")[0].url, "data:image/png;base64,current");
});

test("read failure frees reservations and accepted snapshot preserves images added while sending", async () => {
  let fail = true;
  const drafts = new ImageAttachmentDrafts(async () => { if (fail) throw new Error("Disk read failed"); return "data:image/png;base64,iVBORw0KGgo="; });
  assert.match((await drafts.add("a", [file()]))[0].message, /Disk read failed/); assert.deepEqual(drafts.items("a"), []);
  fail = false; await drafts.add("a", [file("first")]); const sent = drafts.snapshot("a"); await drafts.add("a", [file("later")]);
  drafts.accepted("a", sent); assert.equal(drafts.items("a")[0].name, "later");
});

test("the public file reader refuses unsupported and oversized files before reading bytes", async () => {
  let reads = 0;
  const file = { name: "bad.png", type: "image/png", size: IMAGE_LIMITS.fileBytes + 1, arrayBuffer: async () => { reads++; return new ArrayBuffer(0); } };
  await assert.rejects(readImageFile(file), /4 MiB/);
  await assert.rejects(readImageFile({ ...file, type: "image/svg+xml", size: 8 }), /PNG, JPEG, WebP or GIF/);
  assert.equal(reads, 0);
});


test("file-scoped read failures preserve nested causes and codes while releasing every reservation", async () => {
  const cause = new Error("Fixture device I/O failure");
  const original = Object.assign(new Error("Selected file read failed", { cause }), { code: "EIO" });
  const drafts = new ImageAttachmentDrafts(async () => { throw original; });
  const errors = await drafts.add("a", [file("first.png"), file("second.png")]);
  assert.equal(errors.length, 2);
  for (const [index, error] of errors.entries()) {
    assert.match(error.message, new RegExp(`${index ? "second" : "first"}\\.png: Selected file read failed`));
    assert.equal(error.cause, original); assert.equal(error.cause.code, "EIO"); assert.equal(error.cause.cause, cause);
  }
  assert.deepEqual(drafts.items("a"), []);
});
