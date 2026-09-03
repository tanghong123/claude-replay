// The `/pull` + `/records` protocol's client half is the shared module's (#49): this store
// keeps its timer, its array and its handlers, and applies the plan the reducer returns.
import { cursorText, freshCursor, parseRecords, pullQuery, recordsQuery, reducePull } from "./shared/record-stream.js";

export class RecordStore {
  constructor(handlers) {
    this.handlers = handlers;
    this.session = "";
    this.records = [];
    this.meta = null;
    this.cursor = freshCursor();
    this.generation = 0;
    this.timer = 0;
  }

  open(session) {
    this.stop();
    this.session = session;
    this.records = [];
    this.meta = null;
    this.cursor = freshCursor();
    const generation = ++this.generation;
    this.handlers.reset?.();
    this.poll(generation);
  }

  stop() { clearTimeout(this.timer); this.generation++; }
  cursorText() { return cursorText(this.cursor); }
  recover() {
    this.records = [];
    this.meta = null;
    this.cursor = freshCursor();
    this.handlers.reset?.();
  }

  async poll(generation) {
    if (generation !== this.generation || !this.session) return;
    try {
      const response = await fetch(`/pull?${pullQuery(this.session, this.cursor)}`, { cache: "no-store" });
      if (!response.ok) throw new Error(`pull HTTP ${response.status}`);
      const reply = await response.json();
      if (reply.t === "redirect" && reply.url) { location.assign(reply.url); return; }
      if (reply.t === "error") throw new Error(reply.message || "Session stream unavailable");
      if (reply.committed_ext?.len) {
        const ext = reply.committed_ext;
        const records = await fetch(`/records?${recordsQuery(this.session, ext, reply.epoch)}`, { cache: "no-store" });
        // The pointer raced a store reset. Keep the currently rendered records and the old
        // cursor; the next pull sees the epoch bump and resynchronizes atomically.
        if (records.status === 409) return;
        if (!records.ok) throw new Error(`records HTTP ${records.status}`);
        reply.committed = parseRecords(await records.text());
      } else reply.committed = [];
      if (generation === this.generation) this.apply(reply);
    } catch (error) {
      if (generation === this.generation) this.handlers.error?.(error, this.records.length > 0);
    } finally {
      if (generation === this.generation) this.timer = setTimeout(() => this.poll(generation), 1000);
    }
  }

  apply(reply) {
    const plan = reducePull(this.cursor, this.records.length, reply);
    for (const step of plan.steps) {
      if (step.op === "truncate") this.records.length = step.to;
      else this.records.push(...step.records);
    }
    this.cursor = plan.cursor;
    if (reply.meta) this.meta = reply.meta;
    // The update gate is what the store must REPAINT (changedFrom), not the classic page's
    // idle rule (both zones empty): a shorter provisional zone with nothing to append is idle
    // by that rule yet truncates the store, and the viewport must hear it.
    if (plan.changedFrom !== Infinity || reply.meta) this.handlers.update?.({ records: this.records, meta: this.meta, changedFrom: plan.changedFrom === Infinity ? this.records.length : plan.changedFrom });
  }
}
