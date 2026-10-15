import assert from "node:assert/strict";
import { Projection, plainText, replyContext, viewRecord } from "../src/codex-ui/view-model.js";
import { rawTextHtml } from "../src/codex-ui/components.js";

const record = {
  kind: "user", id: "t1", turn: 1, label: "First user's comment",
  src: "COMPLETE ORIGINAL ENVELOPE",
  body: [{ p: "pre", x: "COMPLETE ORDINARY BODY" }],
  reply_context: {
    version: 1,
    items: [
      { quote: "selected <script>\n  code | table", response: { kind: "comment", text: "First user's comment" } },
      { quote: "second selection", response: { kind: "following_text", text: "last comment\n\n\nrequest-like paragraph" } },
      { quote: "quote only" }
    ],
    context: "\nfile context: note.md\n", request: "Overall request"
  }
};
const view = viewRecord(record);
assert.equal(view.reply.items.length, 3);
assert.ok(view.html.includes("selected &lt;script&gt;\n  code | table"));
assert.equal((view.html.match(/last comment/g) || []).length, 1);
assert.equal((view.html.match(/Overall request/g) || []).length, 1);
assert.ok(view.html.includes('aria-label="Selected earlier text"'));
assert.ok(!view.html.includes("ORDINARY BODY"));
assert.equal(rawTextHtml(record), '<pre class="turn-raw turn-raw-text">COMPLETE ORIGINAL ENVELOPE</pre>');
assert.ok(plainText(record).includes("request-like paragraph"));
assert.ok(!plainText(record).includes("ORDINARY BODY"));

const projection = new Projection();
projection.rebuild([record, { kind: "attachment", id: "image", head: { att_kind: "image", att_name: "shot.png" }, body: [] }], 0);
assert.equal(projection.units.length, 1);
assert.equal(projection.units[0].label, "First user's comment");
assert.equal(projection.units[0].attachments.length, 1);
const quoteOnly = { ...record, label: "selected fallback", reply_context: { version: 1, items: [{ quote: "selected fallback" }] } };
projection.rebuild([quoteOnly], 0);
assert.equal(projection.units[0].label, "selected fallback");
projection.rebuild([{ ...quoteOnly, reply_context: { version: 1, items: [{ quote: "selected fallback", response: { kind: "comment", text: " \n " } }] } }], 0);
assert.equal(projection.units[0].label, "selected fallback", "a response without user-written words uses its selection");
for (const context of [
  undefined, null, {}, { version: 2, items: record.reply_context.items },
  { version: 1, items: [] }, { ...record.reply_context, unknown: true },
  { version: 1, items: [{ quote: "" }] }, { version: 1, items: [{ quote: "a", response: null }] },
  { version: 1, items: [{ quote: "a", response: { kind: "guessed", text: "x" } }] },
  { version: 1, items: [{ quote: "a", response: { kind: "comment", text: "" } }] },
  { ...record.reply_context, request: null }, { ...record.reply_context, context: 3 },
]) {
  const old = { ...record, label: "# Response annotations", reply_context: context };
  assert.equal(replyContext(old), null);
  assert.equal(viewRecord(old).html, "<pre>COMPLETE ORDINARY BODY</pre>");
  assert.ok(plainText(old).includes("COMPLETE ORDINARY BODY"));
  projection.rebuild([old], 0);
  assert.equal(projection.units[0].label, "# Response annotations");
}


const literal = viewRecord({ ...record, reply_context: { version: 1, items: [{ quote: "line\r\n  code", response: { kind: "comment", text: "comment\rtext" } }], request: "request\rtext", context: "context\rtext" } });
assert.ok(literal.html.includes("line&#13;\n  code"));
assert.ok(literal.html.includes("comment&#13;text"));
assert.ok(literal.html.includes("request&#13;text"));
assert.ok(literal.html.includes("context&#13;text"));
assert.ok(rawTextHtml({ src: "\nline\r\n" }).includes("<span></span>\nline&#13;\n"));

// A canonical server label is authoritative, including Unicode and truncation policy.
const unicodeLabel = "🙂".repeat(39) + "a🙂";
projection.rebuild([{ ...record, label: unicodeLabel, reply_context: {version:1,items:[{quote:"selected",response:{kind:"comment",text:unicodeLabel}}]} }], 0);
assert.equal(projection.units[0].label, unicodeLabel);
assert.ok(!/[\uD800-\uDBFF]$/.test(projection.units[0].label));
projection.rebuild([{...record,label:"Canonical preview from Rust"}],0);
assert.equal(projection.units[0].label,"Canonical preview from Rust");
