// The `/pull` + `/records` protocol's client half is the shared module's (#49): this store
// keeps its timer, its array and its handlers, and applies the plan the reducer returns.
import { cursorText, freshCursor, parseRecords, pullQuery, recordsQuery, reducePull } from "./shared/record-stream.js";

/** How long a first pull may take before the page says what it is doing (#221). Long enough that
 *  a warm open — under a second, measured — never shows it; short enough that a cold one does not
 *  look hung. */
const WAITING_AFTER_MS = 1200;

/** The head of a tail-first open is read in pieces of this many bytes of the log, each its own
 *  `/records` range, so a link that drops a long transfer costs one piece and never the whole head
 *  (#s274: a phone over the tailnet climbed to 48% and fell back to 1%, again and again, while one
 *  9.7 MB read restarted from the top after every drop). The server answers a range in whole
 *  records, and a dropped read keeps the records it finished. */
export const HEAD_PIECE_BYTES = 1024 * 1024;

/** How many times in a row the server may REFUSE a read of the head (an HTTP error, or a range that
 *  brings no whole record) before the page starts the session over. A network drop is never
 *  counted: a phone out of signal keeps what it has and tries again every few seconds, however
 *  long. A drop costs the record in flight, so a record larger than what the link carries between
 *  drops would never finish; the largest record measured across hong-devserver's sessions was
 *  682 KB, against drops 3 MB or more apart. */
const HEAD_REFUSALS = 12;

/** A record that has not arrived yet: the HEAD of a tail-first open (#314,
 *  design/tail-first-open.md). One frozen object stands for each of them; the projection folds
 *  the run into a single pending unit, so nothing ever renders one as a record. */
export const PENDING = Object.freeze({ kind: "pending" });

export class RecordStore {
  /** `options.tailBudget` — ask a FIRST pull for the last this-many bytes of the committed log
   *  (the server honours it only on a log over twice that), draw them, and read the head behind
   *  them (#314). Absent: every open reads the whole log first, as it always did. */
  constructor(handlers, options = {}) {
    this.handlers = handlers;
    this.tailBudget = options.tailBudget || 0;
    this.headPieceBytes = options.headPieceBytes || HEAD_PIECE_BYTES;
    this.emptySession = options.emptySession;
    this.sessionFailure = options.sessionFailure;
    // The head still to come after a tail-first open: `{ count, len, epoch }`, or null.
    this.head = null;
    this.session = "";
    this.records = [];
    this.meta = null;
    this.cursor = freshCursor();
    this.generation = 0;
    this.timer = 0;
    // Whether this session's first reply has been applied — `handlers.opened` fires once per open.
    this.shown = false; this.confirmedEmpty = false; this.missingSince = null;
  }

  open(session) {
    this.stop();
    this.session = session;
    this.records = [];
    this.meta = null;
    this.cursor = freshCursor();
    this.shown = false; this.confirmedEmpty = false; this.missingSince = null;
    this.head = null;
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
    this.head = null;
    this.handlers.reset?.();
  }

  /** Show an authoritative native empty session even while its first JSONL pull is pending. */
  refreshEmpty() {
    if (this.records.length || !this.session || !this.emptySession?.(this.session)) return false;
    this.confirmedEmpty = true; this.missingSince = null;
    this.handlers.empty?.();
    if (!this.shown) { this.shown = true; this.handlers.opened?.(); }
    return true;
  }

  /** Surface a definite native read failure without waiting for a missing JSONL pull. */
  refreshFailure() {
    if (this.records.length || !this.session) return false;
    const failure = this.sessionFailure?.(this.session);
    if (!failure) return false;
    this.handlers.error?.(new Error(failure.message), false);
    return true;
  }

  async poll(generation) {
    if (generation !== this.generation || !this.session || this.refreshFailure()) return;
    // #221: the FIRST pull on a session with no cache blocks for as long as the server takes to
    // fold and highlight the whole transcript — measured at 86 s for 400 MB, against 0.4 s once
    // the cache is warm. The page had nothing on it for all of that, which a reader cannot tell
    // from a hang. Say what is happening, but only once the wait is long enough to be a wait: a
    // warm open answers in under a second and must not flash a message.
    let waited = 0;
    if (!this.records.length) {
      waited = setTimeout(() => {
        if (generation !== this.generation || this.refreshFailure() || this.refreshEmpty()) return;
        if (generation === this.generation && !this.records.length) this.handlers.waiting?.();
      }, WAITING_AFTER_MS);
    }
    try {
      // Only a store that holds nothing asks for the tail: a fresh open, or one that recovered.
      const tail = this.tailBudget && !this.records.length ? `&tail=${this.tailBudget}` : "";
      const response = await fetch(`/pull?${pullQuery(this.session, this.cursor)}${tail}`, { cache: "no-store" });
      if (generation !== this.generation) return;
      if (response.status === 404) {
        if (this.refreshEmpty()) return;
        // A turn arrives over the native stream before its first rollout reaches the
        // reader index. Only a previously confirmed empty session gets this grace period.
        if (this.confirmedEmpty && !this.records.length) {
          this.missingSince ??= Date.now();
          if (Date.now() - this.missingSince < 10_000) { this.handlers.materializing?.(); return; }
        }
      }
      if (!response.ok) throw new Error(`pull HTTP ${response.status}`);
      const reply = await response.json();
      if (generation !== this.generation) return;
      this.confirmedEmpty = false; this.missingSince = null;
      if (reply.t === "redirect" && reply.url) { location.assign(reply.url); return; }
      if (reply.t === "error") throw new Error(reply.message || "Session stream unavailable");
      if (reply.committed_ext?.len) {
        const ext = reply.committed_ext;
        const records = await fetch(`/records?${recordsQuery(this.session, ext, reply.epoch)}`, { cache: "no-store" });
        // The pointer raced a store reset. Keep the currently rendered records and the old
        // cursor; the next pull sees the epoch bump and resynchronizes atomically.
        if (records.status === 409) return;
        if (!records.ok) throw new Error(`records HTTP ${records.status}`);
        reply.committed = parseRecords(await this.read(records, ext.len, generation));
      } else reply.committed = [];
      if (generation === this.generation) {
        // A tail-first answer: the committed zone starts past records nobody has sent yet.
        const headCount = !this.records.length && reply.committed_ext?.len && reply.committed_from > 0 ? reply.committed_from : 0;
        this.apply(reply, headCount);
        if (headCount) {
          this.head = { count: headCount, len: reply.committed_ext.offset, epoch: reply.epoch };
          this.loadHead(generation, this.head);
        }
        if (!this.shown) { this.shown = true; this.handlers.opened?.(); }
      }
    } catch (error) {
      if (generation === this.generation) this.handlers.error?.(error, this.records.length > 0);
    } finally {
      clearTimeout(waited);
      if (generation === this.generation) this.timer = setTimeout(() => this.poll(generation), 1000);
    }
  }

  /** The `/records` body as text. On a FIRST read it reports how much of `total` (the range's own
   *  length, so a gzipped reply is measured in the bytes it decodes to) has arrived: a session
   *  opened over a slow link — a phone on the tailnet, #313 — reads tens of MB, and the page can
   *  say how far along it is instead of only that it is waiting. */
  async read(response, total, generation, onProgress = this.records.length ? null : this.handlers.progress) {
    if (!response.body || !onProgress) return response.text();
    const reader = response.body.getReader(), decoder = new TextDecoder(), parts = [];
    let got = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      got += value.byteLength;
      parts.push(decoder.decode(value, { stream: true }));
      if (generation === this.generation) onProgress(got, total);
    }
    parts.push(decoder.decode());
    return parts.join("");
  }

  /** The head of a tail-first open, read behind the tail the page has already drawn (#314). It
   *  replaces the placeholders IN PLACE — record indices never move — and is reported as a
   *  prepend (`prepended`: how many records it brought in front of the tail).
   *
   *  It is read as ranges of at most `headPieceBytes` (#s274), and the server answers each in
   *  whole records (an end inside one is carried on to its end; secret masking reads whole
   *  records). Each range's records are parsed as they complete, so a read that drops keeps every
   *  record it finished and the next range starts at the first one it did not: the progress only
   *  climbs, and a drop costs at most one record's bytes; drops are retried for as long as they
   *  last. A 409 (the log left that epoch), a head that will not parse, a count other than the one
   *  the pull named, or a server that keeps refusing (`HEAD_REFUSALS`) starts the session over; a
   *  head is never spliced in. */
  async loadHead(generation, head) {
    const current = () => generation === this.generation && this.head === head;
    head.records ??= [];
    head.got ??= 0;
    head.shown ??= 0;
    const show = got => {
      head.shown = Math.max(head.shown, got);
      this.handlers.headProgress?.(head.shown, head.len);
    };
    try {
      while (head.got < head.len) {
        const len = Math.min(this.headPieceBytes, head.len - head.got);
        const response = await fetch(`/records?${recordsQuery(this.session, { offset: head.got, len }, head.epoch)}`, { cache: "no-store" });
        if (!current()) return;
        if (response.status === 409) { this.recover(); return; }
        if (!response.ok) throw new Error(`records HTTP ${response.status}`);
        const before = head.got;
        // Every record the bytes so far complete is parsed and kept; the bytes after the last
        // newline wait, as chunks, for the rest of their record.
        let pending = [], pendingLength = 0;
        const take = bytes => {
          const cut = bytes.lastIndexOf(10);
          if (cut < 0) { pending.push(bytes); pendingLength += bytes.length; return; }
          const whole = new Uint8Array(pendingLength + cut + 1);
          let at = 0;
          for (const chunk of pending) { whole.set(chunk, at); at += chunk.length; }
          whole.set(bytes.subarray(0, cut + 1), at);
          for (const record of parseRecords(new TextDecoder().decode(whole))) head.records.push(record);
          head.got += whole.length;
          pending = [bytes.slice(cut + 1)]; pendingLength = bytes.length - cut - 1;
        };
        try {
          if (!response.body) {
            const bytes = new Uint8Array(await response.arrayBuffer());
            if (!current()) return;
            take(bytes);
            show(head.got);
          } else {
            const reader = response.body.getReader();
            for (;;) {
              const { done, value } = await reader.read();
              if (!current()) { reader.cancel().catch(() => {}); return; }
              if (done) break;
              take(value);
              show(head.got + pendingLength);
            }
          }
        } catch (error) {
          // What the read finished is kept; a record it was part way through is read again.
          if (head.got > before) { head.refusals = 0; head.drops = 0; }
          throw error;
        }
        if (head.got === before) throw new Error("records: a range brought no whole record");
        head.refusals = 0; head.drops = 0;
      }
      if (head.records.length !== head.count) { this.recover(); return; }
      for (let i = 0; i < head.records.length; i++) this.records[i] = head.records[i];
      this.head = null;
      this.handlers.update?.({ records: this.records, meta: this.meta, changedFrom: 0, prepended: head.count });
    } catch (error) {
      if (!current()) return;
      // A head that does not parse is not this head.
      if (error instanceof SyntaxError) { this.recover(); return; }
      // A network failure (fetch and a dropped body both reject with a TypeError) is retried for as
      // long as it lasts; a server that keeps refusing is given up on, and the session starts over.
      const dropped = error instanceof TypeError;
      if (dropped) head.drops = (head.drops || 0) + 1;
      else head.refusals = (head.refusals || 0) + 1;
      if (head.refusals > HEAD_REFUSALS) { this.recover(); return; }
      const tries = (head.drops || 0) + (head.refusals || 0);
      this.handlers.headFailed?.(error, tries);
      setTimeout(() => { if (current()) this.loadHead(generation, head); }, Math.min(8000, 1000 * tries));
    }
  }

  apply(reply, headCount = 0) {
    const plan = reducePull(this.cursor, this.records.length, reply);
    // A resync drops everything, a head still in flight with it: that head belongs to a log the
    // server no longer has.
    if (plan.resync && !headCount) this.head = null;
    for (const step of plan.steps) {
      if (step.op === "truncate") this.records.length = step.to;
      else this.records.push(...step.records);
    }
    // The shared plan truncated the empty store to `headCount` before appending the tail, which
    // leaves that many empty slots: they are the head, still to come.
    for (let i = 0; i < headCount; i++) this.records[i] = PENDING;
    this.cursor = plan.cursor;
    if (reply.meta) this.meta = reply.meta;
    // The update gate is what the store must REPAINT (changedFrom), not the classic page's
    // idle rule (both zones empty): a shorter provisional zone with nothing to append is idle
    // by that rule yet truncates the store, and the viewport must hear it.
    if (plan.changedFrom !== Infinity || reply.meta) this.handlers.update?.({ records: this.records, meta: this.meta, changedFrom: plan.changedFrom === Infinity ? this.records.length : plan.changedFrom });
  }
}
