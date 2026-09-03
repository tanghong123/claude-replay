export class RecordStore {
  constructor(handlers) {
    this.handlers = handlers;
    this.session = "";
    this.records = [];
    this.meta = null;
    this.cursor = { epoch: 0, committed: 0, gen: 0, index: 0 };
    this.generation = 0;
    this.timer = 0;
  }

  open(session) {
    this.stop();
    this.session = session;
    this.records = [];
    this.meta = null;
    this.cursor = { epoch: 0, committed: 0, gen: 0, index: 0 };
    const generation = ++this.generation;
    this.handlers.reset?.();
    this.poll(generation);
  }

  stop() { clearTimeout(this.timer); this.generation++; }
  cursorText() { const c = this.cursor; return `${c.epoch}.${c.committed}.${c.gen}.${c.index}`; }
  recover() {
    this.records = [];
    this.meta = null;
    this.cursor = { epoch: 0, committed: 0, gen: 0, index: 0 };
    this.handlers.reset?.();
  }

  async poll(generation) {
    if (generation !== this.generation || !this.session) return;
    try {
      const response = await fetch(`/pull?session=${encodeURIComponent(this.session)}&cursor=${encodeURIComponent(this.cursorText())}`, { cache: "no-store" });
      if (!response.ok) throw new Error(`pull HTTP ${response.status}`);
      const reply = await response.json();
      if (reply.t === "redirect" && reply.url) { location.assign(reply.url); return; }
      if (reply.t === "error") throw new Error(reply.message || "Session stream unavailable");
      if (reply.committed_ext?.len) {
        const ext = reply.committed_ext;
        const records = await fetch(`/records?session=${encodeURIComponent(this.session)}&from=${ext.offset}&len=${ext.len}&epoch=${reply.epoch}`, { cache: "no-store" });
        // The pointer raced a store reset. Keep the currently rendered records and the old
        // cursor; the next pull sees the epoch bump and resynchronizes atomically.
        if (records.status === 409) return;
        if (!records.ok) throw new Error(`records HTTP ${records.status}`);
        reply.committed = (await records.text()).split("\n").filter(Boolean).map(JSON.parse);
      } else reply.committed = [];
      if (generation === this.generation) this.apply(reply);
    } catch (error) {
      if (generation === this.generation) this.handlers.error?.(error, this.records.length > 0);
    } finally {
      if (generation === this.generation) this.timer = setTimeout(() => this.poll(generation), 1000);
    }
  }

  apply(reply) {
    const c = this.cursor;
    let changedFrom = Infinity;
    if (reply.epoch !== c.epoch) {
      this.records.length = 0;
      c.committed = 0;
      c.index = 0;
      changedFrom = 0;
    }
    if (reply.committed.length) {
      changedFrom = Math.min(changedFrom, reply.committed_from);
      this.records.length = reply.committed_from;
      c.committed = reply.committed_from;
      for (const record of reply.committed) { this.records.push(record); c.committed++; }
    }
    const provisionalAt = c.committed + reply.provisional_from;
    if (this.records.length !== provisionalAt || reply.provisional.length) {
      changedFrom = Math.min(changedFrom, provisionalAt);
      this.records.length = provisionalAt;
      this.records.push(...reply.provisional);
    }
    c.epoch = reply.epoch;
    c.gen = reply.provisional_gen;
    c.index = reply.provisional_from + reply.provisional.length;
    if (reply.meta) this.meta = reply.meta;
    if (changedFrom !== Infinity || reply.meta) this.handlers.update?.({ records: this.records, meta: this.meta, changedFrom: changedFrom === Infinity ? this.records.length : changedFrom });
  }
}
